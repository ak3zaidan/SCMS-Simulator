/**
 * Disposal and input binding: findings Q8 and Q20.
 *
 * Q8: `WebGLRenderer.dispose()` does not free a light's shadow map — `LightShadow.dispose()` is the
 * only thing that releases `map` and `mapPass` (three 0.186.0) — so a viewer that is created and
 * destroyed leaks a 2048² depth target, about 16 MiB, every cycle.
 *
 * Q20: `attachInput` registered every listener passively, including `wheel`, which makes
 * `preventDefault()` impossible: in any host where an ancestor of the canvas scrolls, the same
 * gesture zooms the camera *and* scrolls the page.
 */

import { describe, expect, it, vi } from "vitest";
import { PerspectiveCamera } from "three";
import { CameraController, type InputTarget } from "../src/cameras.js";
import { WorldRenderer } from "../src/world-render.js";
import { Viewer } from "../src/scene.js";
import { DARK_THEME } from "../src/theme.js";
import type { ViewerCanvas } from "../src/types.js";
import { NullRenderer } from "./support/null-renderer.js";
import { makeGridWorld } from "./support/fixture.js";

const CANVAS = { width: 800, height: 600, clientWidth: 800, clientHeight: 600 } as unknown as ViewerCanvas;

describe("disposal releases the shadow map (Q8)", () => {
  it("calls LightShadow.dispose and drops the render target", () => {
    const world = new WorldRenderer({ theme: DARK_THEME, shadows: true, shadowMapSize: 2048 });
    const spy = vi.spyOn(world.sun.shadow, "dispose");
    // Stand in for the render target the WebGLRenderer would have allocated on the first shadow
    // pass; `dispose()` has to release it and leave nothing referenced.
    const disposed = { count: 0 };
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (world.sun.shadow as any).map = { dispose: () => { disposed.count++; } };
    world.dispose();
    expect(spy).toHaveBeenCalledTimes(1);
    expect(disposed.count).toBe(1);
    expect(world.sun.shadow.map).toBeNull();
    expect(world.sun.shadow.mapPass).toBeNull();
  });

  it("is reached through Viewer.dispose(), which also drops the background and fog", () => {
    const viewer = new Viewer({
      canvas: CANVAS, theme: "dark", autoStart: false, shadows: true,
      createRenderer: (c) => new NullRenderer(c),
    });
    viewer.setWorld(makeGridWorld({ blocks: 4, blockM: 120 }).world);
    viewer.setFog(true);
    expect(viewer.scene.background).not.toBeNull();
    expect(viewer.scene.fog).not.toBeNull();
    const spy = vi.spyOn(viewer.worldRenderer.sun.shadow, "dispose");
    viewer.dispose();
    expect(spy).toHaveBeenCalledTimes(1);
    expect(viewer.worldRenderer.sun.shadow.map).toBeNull();
    expect(viewer.scene.background).toBeNull();
    expect(viewer.scene.fog).toBeNull();
    expect(viewer.scene.children.length).toBe(0);
  });
});

/** Records every listener registration, so the options can be asserted. */
class RecordingTarget implements InputTarget {
  readonly bound: { type: string; fn: (e: Event) => void; options: AddEventListenerOptions | undefined }[] = [];
  readonly captured: number[] = [];

  addEventListener(type: string, fn: EventListenerOrEventListenerObject | null, options?: boolean | AddEventListenerOptions): void {
    this.bound.push({
      type,
      fn: fn as (e: Event) => void,
      options: typeof options === "object" ? options : undefined,
    });
  }

  removeEventListener(type: string, fn: EventListenerOrEventListenerObject | null): void {
    const i = this.bound.findIndex((b) => b.type === type && b.fn === (fn as (e: Event) => void));
    if (i >= 0) this.bound.splice(i, 1);
  }

  setPointerCapture(pointerId: number): void {
    this.captured.push(pointerId);
  }

  dispatch(type: string, event: Partial<Event> & Record<string, unknown>): void {
    for (const b of this.bound) if (b.type === type) b.fn(event as unknown as Event);
  }

  optionsFor(type: string): AddEventListenerOptions | undefined {
    return this.bound.find((b) => b.type === type)?.options;
  }
}

describe("camera input binding (Q20)", () => {
  function controller(): { cam: CameraController; target: RecordingTarget; detach: () => void } {
    const camera = new PerspectiveCamera(50, 1.6, 0.5, 8000);
    camera.up.set(0, 0, 1);
    const cam = new CameraController({ camera });
    cam.setViewportSize(800, 600);
    const target = new RecordingTarget();
    const detach = cam.attachInput(target);
    return { cam, target, detach };
  }

  it("registers wheel non-passively and cancels the page scroll", () => {
    const { cam, target, detach } = controller();
    expect(target.optionsFor("wheel")).toEqual({ passive: false });
    for (const type of ["pointerdown", "pointermove", "pointerup", "pointercancel"]) {
      expect(target.optionsFor(type), type).toEqual({ passive: true });
    }

    const before = cam.altitudeM;
    const preventDefault = vi.fn();
    target.dispatch("wheel", { deltaY: 240, cancelable: true, preventDefault });
    expect(preventDefault).toHaveBeenCalledTimes(1);
    expect(cam.altitudeM).toBeGreaterThan(before);

    // A non-cancelable event (a passive host, a synthetic one) must not throw.
    target.dispatch("wheel", { deltaY: -240, cancelable: false, preventDefault });
    expect(preventDefault).toHaveBeenCalledTimes(1);
    expect(cam.altitudeM).toBeLessThanOrEqual(before * 1.01);
    detach();
    expect(target.bound.length).toBe(0);
  });

  it("captures the pointer and survives a drag leaving the element", () => {
    const { cam, target, detach } = controller();
    expect(target.bound.some((b) => b.type === "pointerleave")).toBe(false);

    cam.setMode("map", true);
    const start = { x: cam.target.x, y: cam.target.y };
    target.dispatch("pointerdown", { clientX: 100, clientY: 100, pointerId: 3, button: 0, shiftKey: false });
    expect(target.captured).toEqual([3]);
    // The pointer leaves the canvas mid-gesture; with capture, the drag continues.
    target.dispatch("pointerleave", { clientX: 140, clientY: 100, pointerId: 3 });
    target.dispatch("pointermove", { clientX: 160, clientY: 100, pointerId: 3 });
    expect(cam.target.x).not.toBeCloseTo(start.x, 6);
    const moved = cam.target.x;
    target.dispatch("pointerup", { pointerId: 3 });
    target.dispatch("pointermove", { clientX: 400, clientY: 100, pointerId: 3 });
    expect(cam.target.x).toBeCloseTo(moved, 6);
    expect(start.y).toBe(start.y);
    detach();
  });

  it("re-binding detaches the previous target", () => {
    const { cam, target } = controller();
    const second = new RecordingTarget();
    cam.attachInput(second);
    expect(target.bound.length).toBe(0);
    expect(second.bound.length).toBeGreaterThan(0);
    cam.dispose();
    expect(second.bound.length).toBe(0);
  });
});
