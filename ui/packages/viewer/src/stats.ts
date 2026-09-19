/**
 * Frame-time and draw-call accounting for the 09-ui §4 performance budget
 * (60 fps with 5,000 rendered vehicles on an integrated GPU).
 *
 * Everything is kept in a preallocated ring; {@link FrameStats.end} allocates nothing. Only
 * {@link FrameStats.snapshot} builds an object, and it is meant to be called at HUD rate (≤ 5 Hz),
 * not per frame.
 */

import type { RendererInfoLike } from "./types.js";

/** A point-in-time reading of the frame budget. */
export interface FrameStatsSnapshot {
  /** Frames measured since the last {@link FrameStats.reset}. */
  readonly frames: number;
  /** Instantaneous frames per second, from the last frame's duration. */
  readonly fps: number;
  /** Frames per second over the whole window. */
  readonly fpsAverage: number;
  /** Last frame's wall time, milliseconds. */
  readonly frameMs: number;
  readonly meanMs: number;
  readonly minMs: number;
  readonly maxMs: number;
  readonly p50Ms: number;
  readonly p95Ms: number;
  readonly p99Ms: number;
  /** Time inside the viewer's own CPU work (interpolation, culling, matrix writes). */
  readonly cpuMs: number;
  readonly cpuMeanMs: number;
  /** Time inside `renderer.render`. */
  readonly renderMs: number;
  readonly renderMeanMs: number;
  /** Draw calls the renderer issued for the last frame. */
  readonly drawCalls: number;
  readonly triangles: number;
  readonly lines: number;
  readonly points: number;
  readonly geometries: number;
  readonly textures: number;
  /** Actor instances written into instance buffers for the last frame. */
  readonly actorInstances: number;
  /** Actor slots skipped by CPU frustum culling for the last frame. */
  readonly actorCulled: number;
  /** Live actor slots the viewer knew about for the last frame. */
  readonly actorLive: number;
  /** Building instances visible after LOD and culling, if the world renderer reported them. */
  readonly buildingsVisible: number;
  /** Window length in frames. */
  readonly window: number;
}

/** Per-frame counters the subsystems push in before {@link FrameStats.end}. */
export interface FrameCounters {
  actorInstances: number;
  actorCulled: number;
  actorLive: number;
  buildingsVisible: number;
}

const EMPTY_INFO: RendererInfoLike = {
  render: { calls: 0, triangles: 0, frame: 0, lines: 0, points: 0 },
  memory: { geometries: 0, textures: 0 },
};

/**
 * A fixed-window frame-time and draw-call counter.
 *
 * ```ts
 * stats.begin(nowMs);
 * // ... viewer CPU work ...
 * stats.markCpu(nowMs2);
 * renderer.render(scene, camera);
 * stats.end(nowMs3, renderer.info);
 * ```
 */
export class FrameStats {
  readonly window: number;
  readonly counters: FrameCounters = {
    actorInstances: 0, actorCulled: 0, actorLive: 0, buildingsVisible: 0,
  };

  #frameMs: Float32Array;
  #cpuMs: Float32Array;
  #renderMs: Float32Array;
  #sorted: Float32Array;
  #write = 0;
  #filled = 0;
  #frames = 0;
  #startMs = 0;
  #cpuMarkMs = 0;
  #sumFrame = 0;
  #sumCpu = 0;
  #sumRender = 0;
  #info: RendererInfoLike = EMPTY_INFO;

  constructor(window = 240) {
    this.window = Math.max(8, window | 0);
    this.#frameMs = new Float32Array(this.window);
    this.#cpuMs = new Float32Array(this.window);
    this.#renderMs = new Float32Array(this.window);
    this.#sorted = new Float32Array(this.window);
  }

  /** Total frames measured since construction or the last {@link reset}. */
  get frames(): number {
    return this.#frames;
  }

  /** Duration of the most recent completed frame, milliseconds. */
  get lastFrameMs(): number {
    const i = (this.#write + this.window - 1) % this.window;
    return this.#filled === 0 ? 0 : this.#frameMs[i];
  }

  /** Start timing a frame. */
  begin(nowMs: number): void {
    this.#startMs = nowMs;
    this.#cpuMarkMs = nowMs;
    this.counters.actorInstances = 0;
    this.counters.actorCulled = 0;
    this.counters.actorLive = 0;
    this.counters.buildingsVisible = 0;
  }

  /** Mark the boundary between viewer CPU work and the renderer submit. */
  markCpu(nowMs: number): void {
    this.#cpuMarkMs = nowMs;
  }

  /** Finish timing a frame and fold in the renderer's own counters. */
  end(nowMs: number, info?: RendererInfoLike): void {
    const total = nowMs - this.#startMs;
    const cpu = this.#cpuMarkMs - this.#startMs;
    const render = nowMs - this.#cpuMarkMs;
    const i = this.#write;
    if (this.#filled === this.window) {
      this.#sumFrame -= this.#frameMs[i];
      this.#sumCpu -= this.#cpuMs[i];
      this.#sumRender -= this.#renderMs[i];
    } else {
      this.#filled++;
    }
    this.#frameMs[i] = total;
    this.#cpuMs[i] = cpu;
    this.#renderMs[i] = render;
    this.#sumFrame += total;
    this.#sumCpu += cpu;
    this.#sumRender += render;
    this.#write = (i + 1) % this.window;
    this.#frames++;
    if (info) this.#info = info;
  }

  /** Drop the window. */
  reset(): void {
    this.#write = 0;
    this.#filled = 0;
    this.#frames = 0;
    this.#sumFrame = 0;
    this.#sumCpu = 0;
    this.#sumRender = 0;
    this.#frameMs.fill(0);
    this.#cpuMs.fill(0);
    this.#renderMs.fill(0);
  }

  /** `q` in `[0, 1]`; 0.5 is the median. Sorts into a preallocated scratch array. */
  percentileMs(q: number): number {
    const n = this.#filled;
    if (n === 0) return 0;
    const s = this.#sorted.subarray(0, n);
    s.set(this.#frameMs.subarray(0, n));
    s.sort();
    const idx = Math.min(n - 1, Math.max(0, Math.round(q * (n - 1))));
    return s[idx];
  }

  /** Build a plain snapshot. Allocates one object; call it at HUD rate, not per frame. */
  snapshot(): FrameStatsSnapshot {
    const n = this.#filled;
    let min = Infinity;
    let max = 0;
    for (let i = 0; i < n; i++) {
      const v = this.#frameMs[i];
      if (v < min) min = v;
      if (v > max) max = v;
    }
    if (n === 0) {
      min = 0;
      max = 0;
    }
    const mean = n === 0 ? 0 : this.#sumFrame / n;
    const last = this.lastFrameMs;
    const r = this.#info.render;
    const m = this.#info.memory;
    return {
      frames: this.#frames,
      fps: last > 0 ? 1000 / last : 0,
      fpsAverage: mean > 0 ? 1000 / mean : 0,
      frameMs: last,
      meanMs: mean,
      minMs: min,
      maxMs: max,
      p50Ms: this.percentileMs(0.5),
      p95Ms: this.percentileMs(0.95),
      p99Ms: this.percentileMs(0.99),
      cpuMs: n === 0 ? 0 : this.#cpuMs[(this.#write + this.window - 1) % this.window],
      cpuMeanMs: n === 0 ? 0 : this.#sumCpu / n,
      renderMs: n === 0 ? 0 : this.#renderMs[(this.#write + this.window - 1) % this.window],
      renderMeanMs: n === 0 ? 0 : this.#sumRender / n,
      drawCalls: r.calls,
      triangles: r.triangles,
      lines: r.lines,
      points: r.points,
      geometries: m.geometries,
      textures: m.textures,
      actorInstances: this.counters.actorInstances,
      actorCulled: this.counters.actorCulled,
      actorLive: this.counters.actorLive,
      buildingsVisible: this.counters.buildingsVisible,
      window: n,
    };
  }
}
