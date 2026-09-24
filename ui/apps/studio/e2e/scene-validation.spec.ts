/**
 * Visual validation: the properties a picture of the simulator has to have, checked against the
 * real framebuffer in a real browser.
 *
 * Three things the owner saw and reported are the reason this file exists:
 *
 *   1. "I don't really see any cars moving" — the aerial view drew the network and the buildings
 *      and no vehicles.
 *   2. "the camera is positioned weird" — the chase view showed two dark planes and a yellow line,
 *      with no car in sight.
 *   3. `radios 0` in the inspector, beside `bytes_air 1468 B/s` in the panel next to it.
 *
 * None of the three is a crash, a console error or a failing unit test. All three are statements
 * about pixels, or about numbers printed next to pixels, and all three went unnoticed until somebody
 * looked. So this suite looks: it reads the WebGL drawing buffer, and it compares what it finds
 * there with where the scene graph says things are.
 *
 * It is not a pixel diff against a golden image. A golden image goes red on every legitimate change
 * and says nothing about whether the picture is right. Every assertion here is either
 *
 *   • *differential* — hide one group of objects, redraw, and require the pixels to change in the
 *     place the projection predicted, which survives a new road colour or a different building LOD;
 *     or
 *   • *relational* — a number the interface prints must equal the number in the stream it describes.
 *
 * The differential measurements are taken on a **paused** run. A live scene moves between two reads
 * of the framebuffer, and that motion reads as a difference: one measurement here showed 24% of a
 * patch changing between two identical draws of a running scene. Every probe therefore takes three
 * reads — visible, hidden, visible again — and reports both the difference the hiding made and the
 * difference two identical draws made, so the noise floor is part of the result rather than an
 * assumption about it.
 *
 * The companion suite in `packages/viewer/test/scene-correctness.test.ts` carries the half that
 * needs no GPU: camera placement, instance accounting, framing geometry.
 */

import { expect, test, type Page } from "@playwright/test";

/** How far a channel must move for a pixel to count as changed. Above sRGB dither, below a car. */
const PIXEL_DELTA = 8;

/** Side of the square sampled around a projected subject, in device pixels. */
const PATCH = 96;

/** The one `problem` a probe reports that means "there is no subject", not "the subject is lost". */
const NOTHING_FOLLOWED = "nothing is being followed";

/**
 * The smallest a vehicle mark may be at map altitude and still read as an object, in device pixels.
 *
 * Forty is about a seven-pixel square. Below that a vehicle is a speck of noise on a dark road
 * rather than a thing on a map, which is precisely the owner's report — the network and the
 * buildings drew, and the traffic did not read. A mark is not a projected body: at 1,700 m a
 * 4.5 m car projects to two or three pixels however correctly it is drawn, so the fix is a
 * screen-space minimum size, not a bigger car.
 */
const MIN_MAP_MARK_PX = 40;

type HideTarget = "actors" | "buildings";

interface Differential {
  /** Fraction of the sampled region that changed when the target was hidden. */
  readonly signal: number;
  /** Fraction that changed between two *identical* draws — the noise floor of this measurement. */
  readonly noise: number;
  readonly samples: number;
}

interface Placement extends Differential {
  /** Why no placement could be computed, or null when one could. */
  readonly problem: string | null;
  /** Normalised device coordinates of the subject, x and y in [-1, 1], y up. */
  readonly ndcX: number;
  readonly ndcY: number;
  readonly ndcZ: number;
  readonly inFrame: boolean;
  /** `tagName.class` of the topmost element at the subject's centre — the canvas, or what covers it. */
  readonly topmost: string;
  /**
   * Fraction of the subject's on-screen area that some other element sits in front of.
   *
   * A vehicle occupies a region, not a point, and the floating HUD's top edge lands close enough to
   * where a chase camera puts the car that a single-point test flips between runs. Twenty-five
   * points across the sampled patch is stable, and it is also the more honest question: a car with
   * its roof behind a panel is still a car you cannot see properly.
   */
  readonly coveredFraction: number;
  /** What covers it, when anything does. */
  readonly coveredBy: string;
}

interface LumaStats {
  readonly mean: number;
  readonly stdDev: number;
  readonly min: number;
  readonly max: number;
}

/**
 * Hide something, redraw, read the pixels back, put it the way it was, and redraw again.
 *
 * It has to run as one synchronous task: `requestAnimationFrame` must not interleave between the
 * draw and the `readPixels`, because three renders with `preserveDrawingBuffer: false` and the
 * drawing buffer is only guaranteed between a draw and the next composite.
 *
 * `Viewer.renderFrame` rewrites `ActorRenderer.hiddenActorId` from the camera mode at the top of
 * every frame, so setting that field from outside is silently undone — an earlier version of this
 * file did exactly that and measured nothing but scene motion for its trouble. Group visibility is
 * the lever that survives a frame.
 *
 * The overlay layer is switched off for the duration of the probe. Transmit pulses, links and the
 * heatmap animate on the *render* clock, not the simulation clock, so they keep moving on a paused
 * run: they put 20% of a patch in motion between two otherwise identical draws, which swamps the
 * few hundred pixels a vehicle covers at map altitude. They are a separate layer with its own
 * question, and this probe's question is whether the vehicles and the buildings are drawn.
 */
async function differential(
  page: Page,
  target: HideTarget,
  region: { subject: true } | { bandFromTop: number; bandToTop: number },
): Promise<Placement & { luma: LumaStats }> {
  return page.evaluate(
    ({ target, region, delta, patch, nothingFollowed }) => {
      const engine = (window as unknown as { __vwpStudio: { engine: Record<string, unknown> } }).__vwpStudio.engine;
      const viewer = engine.viewer as {
        camera: {
          updateMatrixWorld(): void;
          projectionMatrix: { elements: number[] };
          matrixWorldInverse: { elements: number[] };
        };
        cameras: { followActorId: number | null };
        interpolator: { outActorId: Uint32Array; outOccupied: Uint8Array; outPosition: Float32Array; count: number };
        actors: { group: { visible: boolean } };
        overlays: { group: { visible: boolean } };
        worldRenderer: { buildingsGroup: { visible: boolean } };
        step(dt: number): unknown;
      };
      const canvas = document.querySelector('[data-testid="viewer-canvas"]') as HTMLCanvasElement;
      const gl = (canvas.getContext("webgl2") ?? canvas.getContext("webgl")) as WebGLRenderingContext;
      const dw = canvas.width;
      const dh = canvas.height;
      const node = target === "actors" ? viewer.actors.group : viewer.worldRenderer.buildingsGroup;
      const blank = { ndcX: 0, ndcY: 0, ndcZ: 0, inFrame: false, topmost: "none", coveredFraction: 0, coveredBy: "" };
      const zero = { signal: 0, noise: 0, samples: 0 };
      const noLuma = { mean: 0, stdDev: 0, min: 0, max: 0 };

      let x0 = 0;
      let y0 = 0;
      let pw = dw;
      let ph = dh;
      let place = { problem: null as string | null, ...blank };

      if ("subject" in region) {
        const id = viewer.cameras.followActorId;
        if (id === null) return { problem: nothingFollowed, ...blank, ...zero, luma: noLuma };
        let slot = -1;
        for (let i = 0; i < viewer.interpolator.count; i++) {
          if (viewer.interpolator.outOccupied[i] === 1 && viewer.interpolator.outActorId[i] === id >>> 0) {
            slot = i;
            break;
          }
        }
        if (slot < 0) {
          return { problem: `followed actor ${id} is not in the pose buffer`, ...blank, ...zero, luma: noLuma };
        }

        // Project by hand rather than importing three into the page: projectionMatrix ·
        // matrixWorldInverse · point, then divide through by w.
        const cam = viewer.camera;
        cam.updateMatrixWorld();
        const mul = (m: number[], v: number[]): number[] => [
          m[0] * v[0] + m[4] * v[1] + m[8] * v[2] + m[12] * v[3],
          m[1] * v[0] + m[5] * v[1] + m[9] * v[2] + m[13] * v[3],
          m[2] * v[0] + m[6] * v[1] + m[10] * v[2] + m[14] * v[3],
          m[3] * v[0] + m[7] * v[1] + m[11] * v[2] + m[15] * v[3],
        ];
        const p = slot * 3;
        // The centre of the body, not the contact patch: a car is about 1.4 m tall.
        const world = [
          viewer.interpolator.outPosition[p],
          viewer.interpolator.outPosition[p + 1],
          viewer.interpolator.outPosition[p + 2] + 0.7,
          1,
        ];
        const clip = mul(cam.projectionMatrix.elements, mul(cam.matrixWorldInverse.elements, world));
        const w = clip[3] === 0 ? 1e-6 : clip[3];
        const ndcX = clip[0] / w;
        const ndcY = clip[1] / w;
        const ndcZ = clip[2] / w;
        const inFrame = Math.abs(ndcX) <= 1 && Math.abs(ndcY) <= 1 && ndcZ > -1 && ndcZ < 1;

        const box = canvas.getBoundingClientRect();
        const cx = box.left + ((ndcX + 1) / 2) * box.width;
        const cy = box.top + ((1 - ndcY) / 2) * box.height;
        const name = (el: Element | undefined): string => {
          if (!el) return "none";
          const cls = typeof el.className === "string" ? String(el.className).split(" ")[0] : "";
          return `${el.tagName.toLowerCase()}${cls ? `.${cls}` : ""}`;
        };
        const topmost = name(document.elementsFromPoint(cx, cy)[0]);

        // Twenty-five points across the patch, in CSS pixels. `patch` is device pixels, so scale by
        // the ratio between the drawing buffer and the CSS box.
        const cssPerDevice = box.width / Math.max(1, dw);
        const half = (patch * cssPerDevice) / 2;
        let covered = 0;
        let coveredBy = "";
        let sampled = 0;
        for (let iy = 0; iy < 5; iy++) {
          for (let ix = 0; ix < 5; ix++) {
            const px = cx - half + (half * 2 * ix) / 4;
            const py = cy - half + (half * 2 * iy) / 4;
            if (px < box.left || px > box.right || py < box.top || py > box.bottom) continue;
            sampled++;
            const el = document.elementsFromPoint(px, py)[0];
            if (el !== canvas) {
              covered++;
              if (!coveredBy) coveredBy = name(el ?? undefined);
            }
          }
        }

        place = {
          problem: null, ndcX, ndcY, ndcZ, inFrame, topmost,
          coveredFraction: sampled === 0 ? 0 : covered / sampled,
          coveredBy,
        };
        if (!inFrame) return { ...place, ...zero, luma: noLuma };

        pw = patch;
        ph = patch;
        // readPixels counts rows from the bottom of the drawing buffer.
        x0 = Math.max(0, Math.min(dw - patch, Math.round(((ndcX + 1) / 2) * dw) - (patch >> 1)));
        y0 = Math.max(0, Math.min(dh - patch, Math.round(((ndcY + 1) / 2) * dh) - (patch >> 1)));
      } else {
        y0 = Math.max(0, Math.round(dh * (1 - region.bandToTop)));
        ph = Math.max(1, Math.round(dh * (region.bandToTop - region.bandFromTop)));
      }

      const grab = (): Uint8Array => {
        const buf = new Uint8Array(pw * ph * 4);
        gl.readPixels(x0, y0, pw, ph, gl.RGBA, gl.UNSIGNED_BYTE, buf);
        return buf;
      };
      const changed = (a: Uint8Array, b: Uint8Array): number => {
        let n = 0;
        for (let i = 0; i < a.length; i += 4) {
          if (
            Math.abs(a[i] - b[i]) > delta ||
            Math.abs(a[i + 1] - b[i + 1]) > delta ||
            Math.abs(a[i + 2] - b[i + 2]) > delta
          ) {
            n++;
          }
        }
        return n / (a.length / 4);
      };

      // visible → hidden → visible again, with the animated overlays off throughout. The last pair
      // is the noise floor of this very read.
      const overlaysWere = viewer.overlays.group.visible;
      viewer.overlays.group.visible = false;
      viewer.step(1 / 60);
      viewer.step(1 / 60);
      const shown = grab();
      const was = node.visible;
      node.visible = false;
      viewer.step(1 / 60);
      const hidden = grab();
      node.visible = was;
      viewer.step(1 / 60);
      const again = grab();
      viewer.overlays.group.visible = overlaysWere;
      viewer.step(1 / 60);

      let sum = 0;
      let sumSq = 0;
      let min = 255;
      let max = 0;
      const n = shown.length / 4;
      for (let i = 0; i < shown.length; i += 4) {
        // Rec. 709 luma on the display values, which is what "reads as dark" means to an eye.
        const y = 0.2126 * shown[i] + 0.7152 * shown[i + 1] + 0.0722 * shown[i + 2];
        sum += y;
        sumSq += y * y;
        if (y < min) min = y;
        if (y > max) max = y;
      }
      const mean = sum / n;

      return {
        ...place,
        signal: changed(shown, hidden),
        noise: changed(shown, again),
        samples: pw * ph,
        luma: { mean, stdDev: Math.sqrt(Math.max(0, sumSq / n - mean * mean)), min, max },
      };
    },
    { target, region, delta: PIXEL_DELTA, patch: PATCH, nothingFollowed: NOTHING_FOLLOWED },
  );
}

/** Where the followed vehicle lands, and whether it is the vehicle that is drawn there. */
function placeFollowedVehicle(page: Page): Promise<Placement & { luma: LumaStats }> {
  guardAgainstReload(page);
  return differential(page, "actors", { subject: true });
}

/** What the buildings contribute to a horizontal band of the frame, and how that band reads. */
function buildingsInBand(page: Page, fromTop: number, toTop: number): Promise<Differential & { luma: LumaStats }> {
  return differential(page, "buildings", { bandFromTop: fromTop, bandToTop: toTop });
}

interface SceneFacts {
  readonly mode: string;
  readonly followActorId: number | null;
  readonly camera: { x: number; y: number; z: number };
  readonly target: { x: number; y: number; z: number };
  readonly bbox: { minX: number; minY: number; maxX: number; maxY: number; minZ: number; maxZ: number } | null;
  readonly drawn: number;
  readonly culled: number;
  readonly dropped: number;
  readonly live: number;
  readonly hiddenActorId: number;
  readonly statsInstances: number;
  readonly statsLive: number;
  readonly poseOccupied: number;
  readonly nodeTableSize: number;
  readonly worldSites: number;
}

/**
 * Pages that have reloaded since `streaming()` opened them.
 *
 * Every probe in this file reads live state out of `window.__vwpStudio`, so a reload wipes the
 * subject mid-measurement and every assertion afterwards reports a zero with no hint as to why.
 * That is not hypothetical: running this suite against the Vite dev server while another agent was
 * editing `src/` produced a hot reload in the middle of a drag, and `regressions.spec.ts` reported
 * "expected 1 run.seek, received 0" — a perfectly working feature, called broken, for twenty
 * seconds of polling. The same suite passed in half the time with nothing touching the tree.
 *
 * So the reload is caught and named. The standing recommendation, which is not this file's to make,
 * is to point the e2e `webServer` at `vite preview` over a build, where there is no HMR to fire.
 */
const reloads = new WeakMap<Page, number>();

/** Fail with the real cause if the page has reloaded under the test. */
function guardAgainstReload(page: Page): void {
  const n = reloads.get(page) ?? 0;
  expect(
    n,
    "the page reloaded during the test — most likely the dev server hot-reloaded a file that changed while the suite was running; nothing measured after this point is about the app",
  ).toBe(0);
}

/** Everything the scene graph and the stream know, in one round trip. */
async function sceneFacts(page: Page): Promise<SceneFacts> {
  guardAgainstReload(page);
  return page.evaluate(() => {
    const engine = (window as unknown as { __vwpStudio: { engine: Record<string, unknown> } }).__vwpStudio.engine as {
      viewer: {
        cameras: { state(): Record<string, unknown> };
        actors: { stats: Record<string, number>; hiddenActorId: number };
        stats: { snapshot(): Record<string, number> };
      };
      client: { poses: { count: number; occupied: Uint8Array } } | null;
      nodes: Map<number, unknown>;
      world: { bbox: Record<string, number>; sites: { count: number } } | null;
    };
    const v = engine.viewer;
    const s = v.cameras.state() as {
      mode: string;
      followActorId: number | null;
      position: { x: number; y: number; z: number };
      target: { x: number; y: number; z: number };
    };
    const snap = v.stats.snapshot();
    const poses = engine.client?.poses;
    let occupied = 0;
    if (poses) for (let i = 0; i < poses.count; i++) if (poses.occupied[i] === 1) occupied++;
    const bb = engine.world?.bbox;
    return {
      mode: s.mode,
      followActorId: s.followActorId,
      camera: s.position,
      target: s.target,
      bbox: bb
        ? { minX: bb.minXM, minY: bb.minYM, maxX: bb.maxXM, maxY: bb.maxYM, minZ: bb.minZM, maxZ: bb.maxZM }
        : null,
      drawn: v.actors.stats.drawn,
      culled: v.actors.stats.culled,
      dropped: v.actors.stats.dropped,
      live: v.actors.stats.live,
      hiddenActorId: v.actors.hiddenActorId,
      statsInstances: snap.actorInstances,
      statsLive: snap.actorLive,
      poseOccupied: occupied,
      nodeTableSize: engine.nodes.size,
      worldSites: engine.world?.sites.count ?? 0,
    };
  });
}

/** Make a §6 call through the app's own client, so it goes out the way the interface would send it. */
function rpc<T>(page: Page, method: string, params: Record<string, unknown> = {}): Promise<T> {
  return page.evaluate(
    async ([m, p]) => {
      const engine = (
        window as unknown as { __vwpStudio: { engine: { request(m: string, p: unknown): Promise<unknown> } } }
      ).__vwpStudio.engine;
      return engine.request(m as string, p);
    },
    [method, params] as [string, Record<string, unknown>],
  ) as Promise<T>;
}

/** Get to a streaming page with a running run and traffic in the pose buffer. */
async function streaming(page: Page): Promise<void> {
  await page.goto("/");
  reloads.set(page, 0);
  page.on("load", () => reloads.set(page, (reloads.get(page) ?? 0) + 1));
  await expect(page.getByTestId("viewer-canvas")).toBeVisible();
  await expect(page.getByTestId("connection-state")).toHaveAttribute("data-state", "streaming", { timeout: 60_000 });
  await rpc(page, "run.resume").catch(() => undefined);
  await expect
    .poll(
      async () =>
        page.evaluate(
          () => (window as unknown as { __vwpStudio?: { actorCount(): number } }).__vwpStudio?.actorCount() ?? 0,
        ),
      { timeout: 60_000 },
    )
    .toBeGreaterThan(0);
  // A couple of seconds of real frames, so LOD, culling and the interpolator have all settled.
  await page.waitForTimeout(2500);
}

/**
 * How much a 96-pixel patch at the centre of the frame changes between two consecutive draws.
 *
 * Zero on a settled scene — measured at exactly zero over four consecutive draws with the run
 * paused and the camera converged. Anything above that is the scene still moving, and it is the only
 * honest way to know when a differential measurement can be trusted.
 */
async function stillness(page: Page): Promise<number> {
  return page.evaluate((delta: number) => {
    const v = (
      window as unknown as {
        __vwpStudio: { engine: { viewer: { step(dt: number): unknown } } };
      }
    ).__vwpStudio.engine.viewer;
    const canvas = document.querySelector('[data-testid="viewer-canvas"]') as HTMLCanvasElement;
    const gl = (canvas.getContext("webgl2") ?? canvas.getContext("webgl")) as WebGLRenderingContext;
    const patch = 96;
    const x0 = Math.max(0, (canvas.width >> 1) - (patch >> 1));
    const y0 = Math.max(0, (canvas.height >> 1) - (patch >> 1));
    const pw = Math.min(patch, canvas.width - x0);
    const ph = Math.min(patch, canvas.height - y0);
    const grab = (): Uint8Array => {
      const buf = new Uint8Array(pw * ph * 4);
      gl.readPixels(x0, y0, pw, ph, gl.RGBA, gl.UNSIGNED_BYTE, buf);
      return buf;
    };
    v.step(1 / 60);
    const a = grab();
    v.step(1 / 60);
    const b = grab();
    let n = 0;
    for (let i = 0; i < a.length; i += 4) {
      if (Math.abs(a[i] - b[i]) > delta || Math.abs(a[i + 1] - b[i + 1]) > delta || Math.abs(a[i + 2] - b[i + 2]) > delta) {
        n++;
      }
    }
    return n / (a.length / 4);
  }, PIXEL_DELTA);
}

/**
 * Pause the run, then wait until the frame has actually stopped changing.
 *
 * Both halves matter, and neither is enough on its own. Pausing stops the traffic but not the
 * camera: the fly-down from map altitude is an exponential on the *frame* clock, so under software
 * WebGL at 1,680 × 1,050 with 200 actors it can take three times as long in wall time as it does on
 * a GPU. And a camera-motion threshold in metres cannot be right in two modes at once — six metres
 * is nothing at 1,700 m altitude and is most of the frame at nine metres behind a car.
 *
 * So the criterion is the frame itself: keep waiting until two consecutive draws of the middle of
 * the frame are the same. An earlier version waited on metres and measured a 20% noise floor it
 * then blamed on the renderer; with this one the floor is zero.
 */
async function freeze(page: Page, budgetMs = 60_000): Promise<void> {
  await rpc(page, "run.pause").catch(() => undefined);
  const deadline = Date.now() + budgetMs;
  let quiet = 0;
  let moving = 1;
  let where = "the centre of the frame";
  while (Date.now() < deadline) {
    await page.waitForTimeout(700);
    const subject = await placeFollowedVehicle(page);
    if (subject.problem === NOTHING_FOLLOWED) {
      // No subject: the middle of the frame is the best available witness.
      moving = await stillness(page);
      where = "the centre of the frame";
    } else if (subject.problem !== null) {
      // A follow that is not in the pose buffer means the stream has not caught up with the
      // selection. Keep waiting rather than declare the frame still.
      moving = 1;
      where = subject.problem;
    } else if (!subject.inFrame) {
      // A followed vehicle off the edge of the frame means the camera has not arrived yet: the
      // fly-down starts at map altitude with the subject nowhere near the middle.
      moving = 1;
      where = "the followed vehicle, which is still off-frame";
    } else {
      moving = subject.noise;
      where = "the patch around the followed vehicle";
    }
    // Two quiet reads, not one. A single quiet sample can land between two slow camera steps.
    if (moving < 0.005) {
      if (++quiet >= 2) return;
    } else {
      quiet = 0;
    }
  }
  expect(
    moving,
    `the frame never stopped changing: ${(moving * 100).toFixed(1)}% of ${where} moves between two draws`,
  ).toBeLessThan(0.005);
}

/** Follow an actor that has a node behind it, so the HUD and the inspector fill too. */
async function followEquippedActor(page: Page): Promise<number> {
  const id = await page.evaluate(() => {
    const engine = (window as unknown as { __vwpStudio: { engine: Record<string, unknown> } }).__vwpStudio.engine as {
      client: { poses: { count: number; occupied: Uint8Array; actorId: Uint32Array } } | null;
      nodeByActor: Map<number, number>;
      selectActor(id: number | null, mode?: string): Promise<void>;
    };
    const poses = engine.client?.poses;
    if (!poses) return null;
    let fallback: number | null = null;
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
  expect(id, "no live actor to follow").not.toBeNull();
  await expect.poll(async () => (await sceneFacts(page)).mode, { timeout: 30_000 }).toBe("chase");
  return id as number;
}

/** Leave the engine the way `studio.spec.ts` expects to find it: running, at t = 0. */
test.afterEach(async ({ page }) => {
  await rpc(page, "run.seek", { t_ns: 0, pause_after: false }).catch(() => undefined);
  await rpc(page, "run.speed", { speed: 1 }).catch(() => undefined);
  await rpc(page, "run.resume").catch(() => undefined);
});

// ===============================================================================================
// 1. The aerial view
// ===============================================================================================

/**
 * The strict form: at map altitude every live vehicle is a mark on the map.
 *
 * The Studio opens the map focused on the centre of the *world's* bounding box at a fixed 1,400 m
 * extent (`MAP_OPEN_EXTENT_M` in `state/engine.ts`), which has nothing to do with where the traffic
 * is. The viewer already knows how to do better — `Viewer.frameActors` frames the live actors, and
 * the `f` key is bound to it — so this is a choice of opening framing, not a limit of the renderer.
 */
test("the aerial view draws one mark per live vehicle", async ({ page }) => {
  await streaming(page);
  const f = await sceneFacts(page);

  expect(f.mode).toBe("map");
  expect(f.live).toBeGreaterThan(0);
  expect(f.poseOccupied).toBeGreaterThan(0);

  // The scene's own accounting: nothing may go missing between the pose buffer and the instances.
  expect(f.live).toBe(f.poseOccupied);
  expect(f.drawn + f.culled + f.dropped + (f.hiddenActorId >= 0 ? 1 : 0)).toBe(f.live);

  expect(f.culled, `${f.culled} of ${f.live} live vehicles are outside the opening aerial frustum`).toBe(0);
  expect(f.drawn).toBe(f.live);
});

/**
 * The weaker half of the same property, kept separate because it is the one the owner actually hit:
 * a run with vehicles in it may not open on an empty map.
 *
 * On `scenarios/phase1-manhattan.yaml` — one equipped vehicle, three-kilometre world — this is the
 * whole defect: the single vehicle is a kilometre from the bounding box centre, so the opening frame
 * contains none of the one thing the run is about. With the mock engine's 200 actors, some land
 * inside the frame by luck, which is why the strict form above is worth keeping too.
 */
test("the aerial view never opens on an empty map while vehicles are live", async ({ page }) => {
  await streaming(page);
  const f = await sceneFacts(page);
  expect(f.live).toBeGreaterThan(0);
  expect(f.drawn, `${f.live} vehicles are live and the aerial view draws ${f.drawn}`).toBeGreaterThan(0);
  // Most of them, not a lucky few: an aerial view that shows a fifth of the traffic is not an
  // aerial view of the run.
  expect(f.drawn / f.live).toBeGreaterThan(0.9);
});

test("a vehicle in the aerial view puts pixels where the projection says it will", async ({ page }) => {
  await streaming(page);
  // Pick a subject, then go back up to map altitude with the follow kept, so there is a known
  // vehicle whose projected position can be checked against the pixels at that position.
  const id = await followEquippedActor(page);
  await page.evaluate(() => {
    const v = (
      window as unknown as { __vwpStudio: { engine: { viewer: { cameras: { setMode(m: string, i?: boolean): void } } } } }
    ).__vwpStudio.engine.viewer;
    v.cameras.setMode("map", true);
  });
  // The camera flies back up over several seconds; freeze after the move, not before it.
  await freeze(page);

  const p = await placeFollowedVehicle(page);
  expect(p.problem, p.problem ?? "").toBeNull();
  expect(p.inFrame, `vehicle ${id} projects outside the aerial frame at ${p.ndcX.toFixed(2)}, ${p.ndcY.toFixed(2)}`).toBe(
    true,
  );
  // Hiding the vehicles must change the pixels there. If nothing changes, whatever is at that spot
  // is not a vehicle — which is exactly the state the owner photographed.
  expect(p.noise, "the frame is not still enough to measure").toBeLessThan(0.02);
  const markPx = Math.round(p.signal * p.samples);
  expect(markPx, "hiding the vehicles changed nothing where one is drawn").toBeGreaterThan(0);
  expect(p.signal).toBeGreaterThan(p.noise * 4);
  // And it has to be big enough to see. This is the half of "I don't really see any cars moving"
  // that survives after the framing is fixed: the vehicle is in frame, drawn, in the right place,
  // and three pixels across.
  expect(markPx, `the vehicle covers ${markPx} device pixels at map altitude`).toBeGreaterThanOrEqual(
    MIN_MAP_MARK_PX,
  );
});

// ===============================================================================================
// 2. The chase view
// ===============================================================================================

test("the chase view shows the vehicle it follows", async ({ page }) => {
  await streaming(page);
  const id = await followEquippedActor(page);
  await freeze(page);
  const f = await sceneFacts(page);

  expect(f.mode).toBe("chase");
  expect(f.followActorId).toBe(id);
  // In chase view you are behind the car, so the car is drawn. Only the driver's seat hides it.
  expect(f.hiddenActorId).toBe(-1);
  expect(f.drawn).toBeGreaterThan(0);

  const p = await placeFollowedVehicle(page);
  expect(p.problem, p.problem ?? "").toBeNull();
  expect(p.inFrame, `followed vehicle projects off-screen at ${p.ndcX.toFixed(2)}, ${p.ndcY.toFixed(2)}`).toBe(true);
  // Lower middle of the frame: a camera above and behind a car sees it below the centre line.
  expect(Math.abs(p.ndcX)).toBeLessThan(0.6);
  expect(p.ndcY).toBeLessThan(0.2);
  expect(p.ndcY).toBeGreaterThan(-0.95);

  // It is the vehicle that is drawn there, not the road behind it. At chase range a car fills most
  // of a 96-pixel patch, so this threshold is generous by an order of magnitude.
  expect(p.noise, "the frame is not still enough to measure").toBeLessThan(0.02);
  expect(p.signal, "hiding the vehicles changed nothing in the middle of the chase view").toBeGreaterThan(0.1);
});

/**
 * And the viewer is not the only thing in front of it.
 *
 * A correct scene behind an opaque panel is still a picture with no car in it. The floating OBU HUD
 * is anchored to the bottom of the viewport, which is where a chase camera puts the car it is
 * following — so at the window size the owner was using, the car was behind it. That is what "the
 * camera is positioned weird and I don't really see any cars" looks like when the camera is fine.
 *
 * Deliberately a separate test from the framing one: the two have different fixes, and a single red
 * test that could mean either is a test that gets argued with instead of acted on.
 */
/**
 * Wait until the followed vehicle stops moving *within the frame*, without pausing the run.
 *
 * A chase camera rides with the car, so the car's normalised position is nearly constant even
 * while the world streams past — which means stillness in the frame can be reached without
 * stopping the simulation. That matters here: `freeze` pauses the run, a paused run sends no
 * `Telemetry`, and with no telemetry the OBU HUD renders nothing. Measuring whether the HUD covers
 * the car on a frozen frame would have measured an empty viewport and called it clear.
 */
async function settleInFrame(page: Page, budgetMs = 45_000): Promise<Placement & { luma: LumaStats }> {
  const deadline = Date.now() + budgetMs;
  let previous = await placeFollowedVehicle(page);
  let drift = 1;
  while (Date.now() < deadline) {
    await page.waitForTimeout(800);
    const now = await placeFollowedVehicle(page);
    if (now.problem === null && previous.problem === null && now.inFrame) {
      drift = Math.hypot(now.ndcX - previous.ndcX, now.ndcY - previous.ndcY);
    }
    previous = now;
    if (drift < 0.03) return now;
  }
  expect(drift, `the followed vehicle never settled in the frame (${drift.toFixed(3)} ndc per 800 ms)`).toBeLessThan(
    0.03,
  );
  return previous;
}

test("nothing in the interface covers the vehicle the chase view is following", async ({ page }) => {
  // Two window sizes, because this defect is a question of geometry between a fixed-height panel
  // and a viewport, and it therefore appears and disappears with the window. 1,680 × 1,050 is what
  // the e2e configuration uses; 1,280 × 800 is the size the owner's screenshots were taken at, and
  // is where the floating HUD's top edge lands on top of the car. A suite that only ever looked at
  // the larger window would have called this fixed.
  const findings: string[] = [];
  for (const size of [
    { width: 1680, height: 1050 },
    { width: 1280, height: 800 },
  ]) {
    const where = `${size.width}x${size.height}`;
    await page.setViewportSize(size);
    await streaming(page);
    await followEquippedActor(page);

    // The panel under test has to be on screen, or this proves nothing. It appears once the
    // followed node's `Telemetry` arrives (§3.5), which is why the run is left running.
    await expect(page.getByTestId("obu-hud"), `${where}: the OBU HUD never appeared`).toBeVisible({ timeout: 60_000 });
    await expect(page.getByTestId("hud-identity")).toBeVisible({ timeout: 60_000 });

    const p = await settleInFrame(page);
    expect(p.problem, `${where}: ${p.problem}`).toBeNull();
    expect(p.inFrame, `${where}: the followed vehicle is off-screen`).toBe(true);
    if (p.coveredFraction > 0) {
      findings.push(
        `${where}: ${Math.round(p.coveredFraction * 100)}% of the followed vehicle is behind ${p.coveredBy || "another element"}`,
      );
    }
  }
  // Both window sizes are reported, not just the first to fail: knowing whether this is one
  // viewport or every viewport is the difference between a layout tweak and a rethink.
  expect(findings, findings.join("; ")).toEqual([]);
});

test("the dashboard view is inside the car and the chase view is behind it", async ({ page }) => {
  await streaming(page);
  const id = await followEquippedActor(page);
  await freeze(page);
  expect((await sceneFacts(page)).hiddenActorId).toBe(-1);

  const setMode = (mode: string): Promise<void> =>
    page.evaluate((m: string) => {
      const v = (
        window as unknown as {
          __vwpStudio: { engine: { viewer: { cameras: { setMode(m: string, i?: boolean): void } } } };
        }
      ).__vwpStudio.engine.viewer;
      v.cameras.setMode(m, true);
    }, mode);

  await setMode("dashboard");
  await page.waitForTimeout(1500);
  const dash = await sceneFacts(page);
  expect(dash.mode).toBe("dashboard");
  expect(dash.hiddenActorId, "the driver's own body must be the hidden instance").toBe(id);
  expect(dash.live).toBeGreaterThan(0);

  // Back to chase, and the car comes back. An exception that widens silently is how `radios 0`
  // happened; this is the same shape of mistake one layer down.
  await setMode("chase");
  await page.waitForTimeout(1500);
  expect((await sceneFacts(page)).hiddenActorId).toBe(-1);
});

// ===============================================================================================
// 3. Where the camera is allowed to be
// ===============================================================================================

test("the camera is inside the world and looking at what it says it is", async ({ page }) => {
  await streaming(page);
  const map = await sceneFacts(page);
  expect(map.bbox, "no world decoded").not.toBeNull();
  const bb = map.bbox!;
  const span = Math.max(bb.maxX - bb.minX, bb.maxY - bb.minY);

  // Map: over the world, above the tallest thing in it, not in orbit, pointed down.
  expect(map.camera.x).toBeGreaterThan(bb.minX - span);
  expect(map.camera.x).toBeLessThan(bb.maxX + span);
  expect(map.camera.y).toBeGreaterThan(bb.minY - span);
  expect(map.camera.y).toBeLessThan(bb.maxY + span);
  expect(map.camera.z).toBeGreaterThan(bb.maxZ);
  expect(map.camera.z).toBeLessThan(span * 4);
  expect(map.target.z).toBeLessThan(map.camera.z);

  const id = await followEquippedActor(page);
  await freeze(page);
  const chase = await sceneFacts(page);
  expect(chase.followActorId).toBe(id);

  const where = await page.evaluate((actorId: number) => {
    const it = (
      window as unknown as {
        __vwpStudio: {
          engine: {
            viewer: {
              interpolator: {
                count: number;
                outOccupied: Uint8Array;
                outActorId: Uint32Array;
                outPosition: Float32Array;
              };
            };
          };
        };
      }
    ).__vwpStudio.engine.viewer.interpolator;
    for (let i = 0; i < it.count; i++) {
      if (it.outOccupied[i] === 1 && it.outActorId[i] === actorId >>> 0) {
        return { x: it.outPosition[i * 3], y: it.outPosition[i * 3 + 1], z: it.outPosition[i * 3 + 2] };
      }
    }
    return null;
  }, id);
  expect(where, "the followed actor left the pose buffer").not.toBeNull();
  const a = where!;

  // Chase: the target is the followed vehicle, at about eye height above the road, and the camera
  // is within sight of it and inside the world.
  expect(Math.hypot(chase.target.x - a.x, chase.target.y - a.y)).toBeLessThan(4);
  expect(chase.target.z - a.z).toBeGreaterThan(0);
  expect(chase.target.z - a.z).toBeLessThan(4);
  const range = Math.hypot(chase.camera.x - a.x, chase.camera.y - a.y);
  expect(range).toBeGreaterThan(2);
  expect(range).toBeLessThan(60);
  expect(chase.camera.z - a.z).toBeGreaterThan(0.5);
  expect(chase.camera.z - a.z).toBeLessThan(30);
  expect(chase.camera.x).toBeGreaterThan(bb.minX - span);
  expect(chase.camera.x).toBeLessThan(bb.maxX + span);
  expect(chase.camera.y).toBeGreaterThan(bb.minY - span);
  expect(chase.camera.y).toBeLessThan(bb.maxY + span);
});

// ===============================================================================================
// 4. Buildings, at street level, in the top of the frame
// ===============================================================================================

test("buildings fill the upper frame at street level and read as more than a flat wash", async ({ page }) => {
  await streaming(page);
  await followEquippedActor(page);
  await freeze(page);

  // The upper third: at street level in a city, that band is walls, not sky. The unlit-buildings
  // defect looked exactly like an empty horizon, because the walls drew at the background's own
  // luminance and so contributed nothing an eye could separate from it.
  const upper = await buildingsInBand(page, 0.0, 0.34);
  expect(upper.samples).toBeGreaterThan(10_000);
  expect(upper.noise, "the frame is not still enough to measure").toBeLessThan(0.02);

  // Differential: the buildings own a real share of those pixels.
  expect(upper.signal, "hiding the buildings changed almost nothing in the upper frame").toBeGreaterThan(0.05);
  expect(upper.signal).toBeGreaterThan(upper.noise * 4);

  // And what they draw is distinguishable. A band of one flat colour is a band with nothing in it,
  // whatever the scene graph says was submitted.
  //
  // These two are the weaker half of the check, and deliberately kept as a floor rather than
  // relied on. With the buildings removed from the scene entirely, the differential above drops
  // from 0.81 to 0.00 — measured, by removing them — while the luma spread barely moves (12.3 to
  // 15.5, range 188 to 179), because the sky gradient and the road carry a tonal range of their
  // own. The differential is what catches an absent or unlit wall; these two only catch a band
  // painted one single colour.
  expect(upper.luma.stdDev, "the upper frame is a flat wash").toBeGreaterThan(3);
  expect(upper.luma.max - upper.luma.min, "the upper frame has no tonal range").toBeGreaterThan(20);
});

// ===============================================================================================
// 5. Counters that must agree with their stream
// ===============================================================================================

test("the drawn/live read-out agrees with the instances the viewer wrote", async ({ page }) => {
  await streaming(page);
  const f = await sceneFacts(page);
  expect(f.statsInstances).toBe(f.drawn);
  expect(f.statsLive).toBe(f.live);

  // And the same numbers reach the reader. `StatsReadout` is inside the header's Details
  // disclosure — eleven facts on one line was a previous finding — so that is where to look.
  await page.getByTestId("run-details-button").click();
  await expect(page.getByTestId("fps")).toBeVisible({ timeout: 20_000 });
  const printed = ((await page.getByTestId("fps").textContent()) ?? "").replace(/\s+/g, " ");
  const m = /(\d+) drawn \/ (\d+) live/.exec(printed);
  expect(m, `the read-out does not print drawn/live: ${printed}`).not.toBeNull();
  const shownLive = Number(m![2]);
  expect(shownLive, "the read-out says no vehicles are live while the stream is delivering them").toBeGreaterThan(0);
  // The projection refreshes at 5 Hz while the frame loop runs at 60, so the count may be one
  // projection behind — but it may not be a different population of vehicles.
  expect(Math.abs(shownLive - f.live) / f.live, `read-out says ${shownLive} live, scene has ${f.live}`).toBeLessThan(
    0.1,
  );
});

test("the inspector's radio count agrees with the nodes the engine announced", async ({ page }) => {
  await streaming(page);
  await page.getByRole("button", { name: "state", exact: true }).click();
  await expect(page.getByTestId("inspector-empty")).toBeVisible({ timeout: 30_000 });

  // A live run gains radios as vehicles join, so `run.status` read once is a moving number. Bracket
  // it: the client's node table has to sit inside a reading taken before and one taken after.
  const before = await rpc<{ nodes: number }>(page, "run.status");
  const f = await sceneFacts(page);
  const after = await rpc<{ nodes: number }>(page, "run.status");

  expect(before.nodes, "run.status reports no radios at all").toBeGreaterThan(0);
  expect(f.nodeTableSize, "the client's node table is empty while run.status reports radios").toBeGreaterThan(0);
  expect(
    f.nodeTableSize,
    `the client's node table holds ${f.nodeTableSize}, run.status said ${before.nodes}…${after.nodes}`,
  ).toBeGreaterThanOrEqual(Math.min(before.nodes, after.nodes));

  const printed = await page.evaluate(() => {
    const dl = document.querySelector('[data-testid="inspector-empty"] dl.kv');
    if (!dl) return null;
    const terms = Array.from(dl.querySelectorAll("dt"));
    const i = terms.findIndex((t) => (t.textContent ?? "").trim() === "radios");
    if (i < 0) return null;
    return (dl.querySelectorAll("dd")[i]?.textContent ?? "").trim();
  });
  expect(printed, "the inspector does not print a radio count").not.toBeNull();
  // `radios 0` beside a panel reading `bytes_air 1468 B/s` was the defect. A count that contradicts
  // the stream beside it is worse than a blank, so it has to be the count the stream carries.
  const shown = Number(printed!.replace(/[^0-9]/g, ""));
  expect(shown, `the inspector says "radios ${printed}" for a run with ${before.nodes} radios`).toBeGreaterThan(0);
  expect(shown, `the inspector says "radios ${printed}"; the client's node table holds ${f.nodeTableSize}`).toBe(
    f.nodeTableSize,
  );
});

test("the world chip agrees with the world that was decoded", async ({ page }) => {
  await streaming(page);
  const f = await sceneFacts(page);
  const text = ((await page.locator(".chip", { hasText: "RSUs" }).first().textContent()) ?? "").replace(/\s+/g, " ");
  const rsus = /(\d+) RSUs?/.exec(text);
  expect(rsus, `no RSU count in "${text}"`).not.toBeNull();
  expect(Number(rsus![1]), `the chip says "${text}" for a world with ${f.worldSites} sites`).toBe(f.worldSites);
});

// ===============================================================================================
// 6. The same properties at the ends of the run
// ===============================================================================================

/**
 * The owner's screenshots were taken with the run at its end, which is a different state from a run
 * in the middle: the stream has stopped, the interpolator is extrapolating past its newest snapshot,
 * and nothing arrives to correct it. A still frame has to be as correct as a moving one, at both
 * ends.
 */
test("the views are still correct at t = 0 and after the run has finished", async ({ page }) => {
  await streaming(page);

  /**
   * A seek rebuilds the pose buffer from a fresh keyframe, so the actor that was being followed may
   * not exist at the new time — and a chase view of an actor that is not in the run is a legitimate
   * empty frame, not a defect. The subject is therefore chosen again at each stop.
   */
  const check = async (where: string): Promise<void> => {
    const id = await followEquippedActor(page);
    await freeze(page);
    const f = await sceneFacts(page);
    expect(f.live, `${where}: no live vehicles`).toBeGreaterThan(0);
    expect(f.live, `${where}: live count disagrees with the pose buffer`).toBe(f.poseOccupied);
    expect(f.drawn + f.culled + f.dropped + (f.hiddenActorId >= 0 ? 1 : 0), `${where}: vehicles unaccounted for`).toBe(
      f.live,
    );
    expect(f.followActorId, `${where}: the follow was dropped`).toBe(id);
    const p = await placeFollowedVehicle(page);
    expect(p.problem, `${where}: ${p.problem}`).toBeNull();
    expect(p.inFrame, `${where}: the followed vehicle is off-screen`).toBe(true);
    expect(p.noise, `${where}: the frame is not still enough to measure`).toBeLessThan(0.02);
    expect(p.signal, `${where}: nothing is drawn where the followed vehicle is`).toBeGreaterThan(0.05);
  };

  // `freeze` leaves the run paused, and a seek with `pause_after: false` resumes it, so `run.resume`
  // would be refused with −32002 "run is not paused". Ask for the state rather than assume it.
  const resumeIfPaused = async (): Promise<void> => {
    const s = await rpc<{ state: string }>(page, "run.status");
    if (s.state === "paused") await rpc(page, "run.resume");
  };
  await rpc(page, "run.seek", { t_ns: 0, pause_after: false });
  await resumeIfPaused();
  await page.waitForTimeout(2000);
  await check("t = 0");

  // Past the end: the stream stops, the last poses stay, and the view must hold them rather than
  // empty out.
  const status = await rpc<{ t_end_ns: number | string }>(page, "run.status");
  const end = Number(status.t_end_ns ?? 0);
  expect(end).toBeGreaterThan(0);
  await rpc(page, "run.seek", { t_ns: Math.max(0, end - 1_000_000_000), pause_after: false });
  await rpc(page, "run.speed", { speed: 1 });
  await resumeIfPaused();
  await page.waitForTimeout(6000);
  await check("after the end of the run");
});

// ===============================================================================================
// 7. Traffic lights agree with the stream
// ===============================================================================================

/**
 * Install a recorder of every signal row the stream delivers, with the sim time it is for, so the
 * lamps can be checked against the state the engine said held at the instant the scene is drawn.
 */
async function recordSignals(page: Page): Promise<void> {
  await page.evaluate(() => {
    const w = window as unknown as {
      __vwpStudio: { engine: { client: { onKeyframe(f: (k: unknown) => void): () => void; onDelta(f: (d: unknown) => void): () => void } | null } };
      __signalLog?: { t: number; keyframe: boolean; ids: number[]; phases: number[] }[];
    };
    const log: { t: number; keyframe: boolean; ids: number[]; phases: number[] }[] = [];
    w.__signalLog = log;
    const client = w.__vwpStudio.engine.client;
    if (!client) throw new Error("no client");
    const take = (keyframe: boolean) => (m: unknown): void => {
      const msg = m as { simTimeNs: bigint; signals: { count: number; signalId: Uint32Array; phase: Uint8Array } };
      if (!keyframe && msg.signals.count === 0) return;
      log.push({
        t: Number(msg.simTimeNs) / 1e9,
        keyframe,
        ids: Array.from(msg.signals.signalId.subarray(0, msg.signals.count)),
        phases: Array.from(msg.signals.phase.subarray(0, msg.signals.count)),
      });
    };
    client.onKeyframe(take(true));
    client.onDelta(take(false));
  });
}

/**
 * Every head against the state the stream says held at the drawn instant: the latest keyframe at or
 * before it, with every later delta up to it on top. Returns the heads checked and the mismatches.
 */
function signalAgreement(page: Page): Promise<{ heads: number; mismatches: number; examples: string[]; renderSim: number; rows: number }> {
  return page.evaluate(() => {
    const w = window as unknown as {
      __vwpStudio: { engine: { viewer: { interpolator: { renderSimSeconds: number }; worldRenderer: { signals: { count: number; headState(i: number): { signalId: number; phase: number } | null } } } } };
      __signalLog: { t: number; keyframe: boolean; ids: number[]; phases: number[] }[];
    };
    const v = w.__vwpStudio.engine.viewer;
    const at = v.interpolator.renderSimSeconds;
    const log = w.__signalLog;
    let start = -1;
    for (let i = 0; i < log.length; i++) if (log[i].keyframe && log[i].t <= at + 1e-6) start = i;
    // Nothing to compare against until a keyframe at or before the drawn instant has been seen:
    // the recorder starts mid-stream, and deltas alone are not the whole state.
    if (start < 0) return { heads: 0, mismatches: 0, examples: [], renderSim: at, rows: 0 };
    const state = new Map<number, number>();
    if (start >= 0) {
      for (let i = start; i < log.length; i++) {
        const e = log[i];
        if (e.t > at + 1e-6) break;
        if (i > start && e.keyframe) state.clear();
        e.ids.forEach((id, k) => state.set(id, e.phases[k]));
      }
    }
    const s = v.worldRenderer.signals;
    let mismatches = 0;
    const examples: string[] = [];
    for (let i = 0; i < s.count; i++) {
      const h = s.headState(i);
      if (!h) continue;
      const want = state.has(h.signalId) ? (state.get(h.signalId) as number) : 0xff;
      if (h.phase !== want) {
        mismatches++;
        if (examples.length < 5) examples.push(`head ${i} (signal ${h.signalId}) shows ${h.phase}, stream says ${want}`);
      }
    }
    return { heads: s.count, mismatches, examples, renderSim: at, rows: log.length };
  });
}

test("every signal head shows the stream's state for the drawn instant — running, after a seek, and after a reconnect", async ({ page }) => {
  await streaming(page);
  await recordSignals(page);
  // Long enough to see a phase change (the mock's plan changes every few seconds) and a keyframe.
  const findings: string[] = [];
  let checked = 0;
  for (let i = 0; i < 24; i++) {
    await page.waitForTimeout(400);
    const a = await signalAgreement(page);
    if (a.rows === 0) continue;
    checked++;
    if (a.mismatches > 0) findings.push(`running, t=${a.renderSim.toFixed(2)}: ${a.mismatches}/${a.heads} heads wrong — ${a.examples.join("; ")}`);
  }
  expect(checked, "no signal rows arrived at all").toBeGreaterThan(10);

  // A seek backwards, paused there: the keyframe is the whole state, and nothing from later survives.
  await rpc(page, "run.seek", { t_ns: 2_000_000_000, pause_after: true });
  await page.waitForTimeout(2500);
  let a = await signalAgreement(page);
  if (a.mismatches > 0) findings.push(`after a seek back: ${a.mismatches}/${a.heads} heads wrong — ${a.examples.join("; ")}`);

  // A reconnect: a fresh connection sends a keyframe; the lamps must come back to the stream's state.
  await page.evaluate(async () => {
    const e = (window as unknown as { __vwpStudio: { engine: { reopenStream(): Promise<void> } } }).__vwpStudio.engine;
    await e.reopenStream();
  });
  await expect(page.getByTestId("connection-state")).toHaveAttribute("data-state", "streaming", { timeout: 60_000 });
  await recordSignals(page);
  await rpc(page, "run.resume").catch(() => undefined);
  await page.waitForTimeout(3000);
  a = await signalAgreement(page);
  if (a.rows > 0 && a.mismatches > 0) findings.push(`after a reconnect: ${a.mismatches}/${a.heads} heads wrong — ${a.examples.join("; ")}`);
  expect(findings, findings.join("\n")).toEqual([]);
});

// ===============================================================================================
// 8. Vehicles and buildings
// ===============================================================================================

/**
 * No vehicle may be drawn inside a building the viewer draws unless the engine put it there.
 *
 * The old far building LOD was the footprint's bounding box, so from 700 m out a fifth of the
 * traffic sat inside drawn walls; interpolation cutting a corner could do the same up close. This
 * samples every live vehicle for a few seconds against the footprints, in the plan view (every
 * building far) and in chase, and separates the viewer's share from the engine's: a rendered
 * position inside a footprint where none of the stream's last three poses of that vehicle is.
 */
test("no vehicle is drawn inside a building unless the stream put it there", async ({ page }) => {
  await streaming(page);
  const sample = async (): Promise<{ samples: number; viewer: number; engine: number; examples: string[] }> =>
    page.evaluate(async () => {
      const e = (window as unknown as { __vwpStudio: { engine: Record<string, unknown> } }).__vwpStudio.engine as {
        viewer: {
          interpolator: { count: number; outOccupied: Uint8Array; outActorId: Uint32Array; outPosition: Float32Array };
          worldRenderer: { buildingIndexAt(x: number, y: number): number; buildingTopOf(i: number): number; ghostBuilding: number };
        };
        client: { poses: { count: number; occupied: Uint8Array; actorId: Uint32Array; positions: Float32Array }; onDelta(f: () => void): () => void };
      };
      const v = e.viewer;
      const w = v.worldRenderer;
      const inside = (x: number, y: number, z: number): boolean => {
        const b = w.buildingIndexAt(x, y);
        return b >= 0 && b !== w.ghostBuilding && z < w.buildingTopOf(b);
      };
      const history: Map<number, [number, number, number]>[] = [];
      const snap = (): void => {
        const p = e.client.poses;
        const m = new Map<number, [number, number, number]>();
        for (let i = 0; i < p.count; i++) if (p.occupied[i] === 1) m.set(p.actorId[i], [p.positions[i * 3], p.positions[i * 3 + 1], p.positions[i * 3 + 2]]);
        history.push(m);
        if (history.length > 3) history.shift();
      };
      snap();
      const off = e.client.onDelta(snap);
      let samples = 0;
      let viewer = 0;
      let engine = 0;
      const examples: string[] = [];
      const t0 = performance.now();
      while (performance.now() - t0 < 4000) {
        await new Promise((r) => requestAnimationFrame(r));
        const it = v.interpolator;
        for (let i = 0; i < it.count; i++) {
          if (it.outOccupied[i] !== 1) continue;
          samples++;
          const x = it.outPosition[i * 3];
          const y = it.outPosition[i * 3 + 1];
          const z = it.outPosition[i * 3 + 2];
          if (!inside(x, y, z)) continue;
          const id = it.outActorId[i];
          const streamInside = history.some((h) => {
            const q = h.get(id);
            return q !== undefined && inside(q[0], q[1], q[2]);
          });
          if (streamInside) engine++;
          else {
            viewer++;
            if (examples.length < 5) examples.push(`vehicle ${id} drawn at ${x.toFixed(1)},${y.toFixed(1)}`);
          }
        }
      }
      off();
      return { samples, viewer, engine, examples };
    });

  const map = await sample();
  await followEquippedActor(page);
  await page.waitForTimeout(3000);
  const chase = await sample();
  // eslint-disable-next-line no-console
  console.log(`vehicles inside buildings — plan view: ${map.viewer} viewer-caused, ${map.engine} engine-placed of ${map.samples}; chase: ${chase.viewer} and ${chase.engine} of ${chase.samples}`);
  expect(map.samples + chase.samples).toBeGreaterThan(100);
  expect(map.viewer + chase.viewer, [...map.examples, ...chase.examples].join("; ")).toBe(0);
});

// ===============================================================================================
// 9. Depth: no z-fighting between the road's layers
// ===============================================================================================

/**
 * Move the plan-view camera by a centimetre — a hundredth of a pixel at this altitude — and redraw.
 * With the road's layers resolved by the depth buffer nothing changes; z-fighting layers swap
 * which one is in front and the road's surface speckles. Vehicles, overlays and signal lamps are
 * hidden: they are not what this measures.
 */
test("the plan view's road layers do not z-fight as the camera moves", async ({ page }) => {
  await streaming(page);
  await rpc(page, "run.pause").catch(() => undefined);
  const flicker = await page.evaluate(() => {
    const e = (window as unknown as { __vwpStudio: { engine: Record<string, unknown> } }).__vwpStudio.engine as {
      viewer: {
        stop(): void;
        start(): void;
        step(dt: number): unknown;
        camera: { near: number };
        cameras: { setMode(m: string, i?: boolean): unknown; altitudeM: number; target: { x: number; y: number; z: number }; snap(): void };
        actors: { group: { visible: boolean } };
        overlays: { group: { visible: boolean } };
        worldRenderer: { signalsGroup: { visible: boolean }; sitesGroup: { visible: boolean }; buildingsGroup: { visible: boolean } };
      };
    };
    const v = e.viewer;
    v.stop();
    const hide = [v.actors.group, v.overlays.group, v.worldRenderer.signalsGroup, v.worldRenderer.sitesGroup, v.worldRenderer.buildingsGroup];
    const was = hide.map((g) => g.visible);
    hide.forEach((g) => (g.visible = false));
    v.cameras.setMode("map", true);
    v.cameras.altitudeM = 1400;
    v.cameras.snap();
    const canvas = document.querySelector('[data-testid="viewer-canvas"]') as HTMLCanvasElement;
    const gl = (canvas.getContext("webgl2") ?? canvas.getContext("webgl")) as WebGLRenderingContext;
    const grab = (): Uint8Array => {
      const buf = new Uint8Array(canvas.width * canvas.height * 4);
      gl.readPixels(0, 0, canvas.width, canvas.height, gl.RGBA, gl.UNSIGNED_BYTE, buf);
      return buf;
    };
    for (let i = 0; i < 10; i++) v.step(1 / 60);
    // The base frame is drawn exactly like the moved ones — snapped, no time passing — or the
    // plan view's automatic recentring (every 0.2 s of frame time) is measured instead of depth.
    const tx = v.cameras.target.x;
    v.cameras.snap();
    v.step(0);
    const base = grab();
    let worst = 0;
    let total = 0;
    for (let k = 1; k <= 6; k++) {
      v.cameras.target.x = tx + k * 0.01;
      v.cameras.snap();
      v.step(0);
      const f = grab();
      let changed = 0;
      for (let i = 0; i < f.length; i += 4) {
        if (Math.abs(f[i] - base[i]) > 24 || Math.abs(f[i + 1] - base[i + 1]) > 24 || Math.abs(f[i + 2] - base[i + 2]) > 24) changed++;
      }
      const frac = changed / (f.length / 4);
      worst = Math.max(worst, frac);
      total += frac;
    }
    v.cameras.target.x = tx;
    hide.forEach((g, i) => (g.visible = was[i]));
    v.start();
    return { worst, mean: total / 6, near: v.camera.near };
  });
  // eslint-disable-next-line no-console
  console.log(`plan-view flicker under a 1-6 cm camera move: worst ${(flicker.worst * 100).toFixed(3)} % of pixels, mean ${(flicker.mean * 100).toFixed(3)} %, near plane ${flicker.near.toFixed(1)} m`);
  expect(flicker.worst).toBeLessThan(0.002);
});
