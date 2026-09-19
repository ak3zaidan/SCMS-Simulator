/**
 * Camera modes and the controller that animates between them.
 *
 * 09-ui §3 is explicit: "a single `PerspectiveCamera` is used for both views … the transition is an
 * interpolation of (position, target, field of view) with smooth damping, following jevpilot's
 * exponential lerp for camera position and look target (`1 − exp(−4·dt)` and `1 − exp(−6·dt)`)". Those
 * two constants are {@link CameraControllerOptions.positionLambda} and `lookLambda`, and they are why
 * {@link CameraController.flyTo} needs no tweening machinery at all: setting the mode changes the
 * *desired* pose, and the same exponential smoothing that keeps the chase camera behind a car also
 * carries the camera down from 600 m to 9 m behind it, continuously, in about a second. There is no
 * second scene, no reload, and no cut.
 *
 * Top-down is *nearly* straight down rather than exactly: `Object3D.lookAt` is degenerate when the
 * view direction is parallel to `up`, which for a z-up world is precisely the 90° pitch 09-ui asks
 * for. The controller keeps a horizontal offset of `altitude · 1e-4` (10 cm at a 1 km altitude), so
 * the basis is well defined and the picture is indistinguishable from a true plan view.
 */

import { MathUtils, Vector3 } from "three";
import type { PerspectiveCamera } from "three";
import type { CameraMode } from "@vwp/protocol";
import type { WorldRenderer } from "./world-render.js";

export type { CameraMode };

/** The five persistent modes; `jump` is an action, handled as "snap, then chase". */
export const CAMERA_MODES: readonly CameraMode[] = ["map", "chase", "dashboard", "free", "rsu"];

/** Tuning for {@link CameraController}. */
export interface CameraControllerOptions {
  readonly camera: PerspectiveCamera;
  /** Supplies building footprints for {@link CameraController.keepCameraOutsideBuildings}. */
  readonly world?: WorldRenderer | null;
  /** Position smoothing rate; `1 − exp(−λ·dt)`. Default 4 (09-ui §3). */
  readonly positionLambda?: number;
  /** Look-target smoothing rate. Default 6 (09-ui §3). */
  readonly lookLambda?: number;
  /** Field-of-view smoothing rate. Default 5. */
  readonly fovLambda?: number;
  /** Chase distance behind the followed actor, metres. Default 9. */
  readonly chaseDistanceM?: number;
  /** Chase elevation angle, degrees. Default 16. */
  readonly chasePitchDeg?: number;
  /** Height of the chase look target above the actor's origin, metres. Default 1.4. */
  readonly chaseLookHeightM?: number;
  /** Driver's eye height in dashboard mode, metres. Default 1.25. */
  readonly dashboardEyeHeightM?: number;
  /** Starting map altitude, metres. Default 700. */
  readonly mapAltitudeM?: number;
  readonly minAltitudeM?: number;
  readonly maxAltitudeM?: number;
  /** Clearance kept above a roof the camera would otherwise be inside, metres. Default 3. */
  readonly buildingClearanceM?: number;
  /**
   * Longest camera-to-target distance at which the occlusion march runs, metres. Default 60 — a
   * chase or dashboard working distance. Beyond it only the roof lift applies; see
   * {@link CameraController.keepCameraOutsideBuildings}.
   */
  readonly occlusionRangeM?: number;
  /** Field of view per mode, degrees. */
  readonly fovMapDeg?: number;
  readonly fovChaseDeg?: number;
  readonly fovDashboardDeg?: number;
}

/** A serialisable camera state, shaped for §6.7 `view.camera`'s result. */
export interface CameraState {
  readonly mode: CameraMode;
  readonly position: { x: number; y: number; z: number };
  readonly target: { x: number; y: number; z: number };
  readonly fovDeg: number;
  readonly followActorId: number | null;
  readonly altitudeM: number;
  readonly distanceM: number;
  readonly bearingRad: number;
}

/** What {@link CameraController.attachInput} binds to; `HTMLElement`, `Window` and `Document` all fit. */
export type InputTarget = Pick<EventTarget, "addEventListener" | "removeEventListener">;

const DEG = Math.PI / 180;

/**
 * Drives one `PerspectiveCamera` through every viewer mode.
 *
 * The owner calls {@link setFollowPose} each frame with the interpolated pose of the followed actor
 * (the controller never reaches into the pose buffer itself, so replay, live and test all look the
 * same to it), then {@link update} with the frame's `dt`.
 */
export class CameraController {
  readonly camera: PerspectiveCamera;

  #world: WorldRenderer | null;
  #mode: CameraMode = "map";
  #followActorId: number | null = null;
  #followValid = false;
  #rsuIndex = -1;

  /** The point the camera orbits or looks at, in ENU metres. */
  readonly target = new Vector3();
  /** The smoothed look target actually handed to `lookAt`. */
  readonly look = new Vector3();

  #desiredPosition = new Vector3();
  #desiredLook = new Vector3();
  #followPos = new Vector3();
  #followHeading = 0;
  #followSpeed = 0;

  #altitude: number;
  #minAltitude: number;
  #maxAltitude: number;
  #bearing = Math.PI / 2; // looking north
  #orbitYaw = 0;
  #orbitPitch: number;
  #chaseDistance: number;
  #chaseLookHeight: number;
  #eyeHeight: number;
  #clearance: number;
  #occlusionRange: number;
  #freeVelocity = new Vector3();
  #freeYaw = 0;
  #freePitch = -0.3;

  readonly positionLambda: number;
  readonly lookLambda: number;
  readonly fovLambda: number;
  #fovMap: number;
  #fovChase: number;
  #fovDashboard: number;
  #desiredFov: number;

  #viewportW = 1280;
  #viewportH = 720;
  #detach: (() => void) | null = null;
  #scratch = new Vector3();
  #scratchB = new Vector3();

  constructor(options: CameraControllerOptions) {
    this.camera = options.camera;
    this.camera.up.set(0, 0, 1);
    this.#world = options.world ?? null;
    this.positionLambda = options.positionLambda ?? 4;
    this.lookLambda = options.lookLambda ?? 6;
    this.fovLambda = options.fovLambda ?? 5;
    this.#chaseDistance = options.chaseDistanceM ?? 9;
    this.#orbitPitch = (options.chasePitchDeg ?? 16) * DEG;
    this.#chaseLookHeight = options.chaseLookHeightM ?? 1.4;
    this.#eyeHeight = options.dashboardEyeHeightM ?? 1.25;
    this.#altitude = options.mapAltitudeM ?? 700;
    this.#minAltitude = options.minAltitudeM ?? 12;
    this.#maxAltitude = options.maxAltitudeM ?? 20_000;
    this.#clearance = options.buildingClearanceM ?? 3;
    this.#occlusionRange = options.occlusionRangeM ?? 60;
    this.#fovMap = options.fovMapDeg ?? 45;
    this.#fovChase = options.fovChaseDeg ?? 55;
    this.#fovDashboard = options.fovDashboardDeg ?? 68;
    this.#desiredFov = this.#fovMap;
    this.camera.fov = this.#fovMap;
    this.camera.updateProjectionMatrix();
    this.#computeDesired();
    this.snap();
  }

  /** Current mode. */
  get mode(): CameraMode {
    return this.#mode;
  }

  /** Actor the camera follows, or null. */
  get followActorId(): number | null {
    return this.#followActorId;
  }

  /** Map altitude in metres. */
  get altitudeM(): number {
    return this.#altitude;
  }

  set altitudeM(v: number) {
    this.#altitude = MathUtils.clamp(v, this.#minAltitude, this.#maxAltitude);
  }

  /** Chase/orbit distance in metres. */
  get distanceM(): number {
    return this.#chaseDistance;
  }

  set distanceM(v: number) {
    this.#chaseDistance = MathUtils.clamp(v, 2, 400);
  }

  /** Map bearing in radians; `π/2` is north-up. */
  get bearingRad(): number {
    return this.#bearing;
  }

  set bearingRad(v: number) {
    this.#bearing = v;
  }

  /** Attach a world so the camera can avoid building interiors. */
  setWorld(world: WorldRenderer | null): void {
    this.#world = world;
  }

  /** Tell the controller the drawing-buffer size, for pan scaling and the projection aspect. */
  setViewportSize(width: number, height: number): void {
    this.#viewportW = Math.max(1, width);
    this.#viewportH = Math.max(1, height);
    this.camera.aspect = this.#viewportW / this.#viewportH;
    this.camera.updateProjectionMatrix();
  }

  /**
   * Switch mode. The camera animates there; pass `instant` to cut.
   *
   * `"jump"` is treated as "snap to the target, then chase", matching 09-ui §3's description of it as
   * an action rather than a persistent mode.
   */
  setMode(mode: CameraMode, instant = false): void {
    if (mode === "jump") {
      this.#mode = "chase";
      this.#computeDesired();
      this.snap();
      return;
    }
    this.#mode = mode;
    this.#desiredFov = mode === "dashboard" ? this.#fovDashboard : mode === "map" ? this.#fovMap : this.#fovChase;
    if (mode === "free") {
      // Seed the free camera from wherever the camera currently is, so "free" never jumps.
      const dir = this.#scratch.copy(this.look).sub(this.camera.position);
      if (dir.lengthSq() > 1e-6) {
        dir.normalize();
        this.#freeYaw = Math.atan2(dir.y, dir.x);
        this.#freePitch = Math.asin(MathUtils.clamp(dir.z, -1, 1));
      }
      this.#desiredPosition.copy(this.camera.position);
      this.#freeVelocity.set(0, 0, 0);
    }
    this.#computeDesired();
    if (instant) this.snap();
  }

  /** Follow an actor (or nothing). Does not change the mode. */
  follow(actorId: number | null): void {
    this.#followActorId = actorId;
    this.#followValid = false;
  }

  /**
   * The signature interaction of 09-ui §1.2: from the top-down map, click a vehicle and the camera
   * flies down into a chase view of it. One call, no scene switch — the mode change re-aims the
   * desired pose and the exponential smoothing in {@link update} does the rest.
   */
  flyTo(actorId: number, mode: CameraMode = "chase", instant = false): void {
    this.follow(actorId);
    this.setMode(mode, instant);
  }

  /** Watch from an RSU mast. `siteIndex` indexes {@link WorldRenderer.sitePositions}. */
  viewFromSite(siteIndex: number, instant = false): void {
    this.#rsuIndex = siteIndex;
    this.setMode("rsu", instant);
  }

  /** Move the map focus. */
  focusOn(x: number, y: number, z = 0): void {
    this.target.set(x, y, z);
  }

  /** Set the map altitude so the vertical field of view covers `extentM` metres. */
  fitExtent(extentM: number): void {
    const half = Math.max(1, extentM) / 2;
    this.altitudeM = half / Math.tan((this.camera.fov * DEG) / 2);
  }

  /** The extent in metres the map view currently covers vertically. */
  get extentM(): number {
    return 2 * this.#altitude * Math.tan((this.camera.fov * DEG) / 2);
  }

  /** Feed the followed actor's interpolated pose. Call once per frame before {@link update}. */
  setFollowPose(x: number, y: number, z: number, headingRad: number, speedMps: number): void {
    this.#followPos.set(x, y, z);
    this.#followHeading = headingRad;
    this.#followSpeed = speedMps;
    this.#followValid = true;
    if (this.#mode !== "free" && this.#mode !== "rsu") this.target.set(x, y, z);
  }

  /** The followed actor left the stream; the camera holds its last position instead of snapping. */
  clearFollowPose(): void {
    this.#followValid = false;
  }

  /** Free-fly input: a body-frame velocity in metres per second. */
  setFreeVelocity(forward: number, right: number, up: number): void {
    this.#freeVelocity.set(forward, right, up);
  }

  /** Cut to the desired pose with no animation. */
  snap(): void {
    this.#computeDesired();
    this.camera.position.copy(this.#desiredPosition);
    this.look.copy(this.#desiredLook);
    this.camera.fov = this.#desiredFov;
    this.camera.updateProjectionMatrix();
    this.camera.lookAt(this.look);
    this.camera.updateMatrixWorld();
  }

  /**
   * Advance the smoothing by `dt` seconds.
   *
   * `1 − exp(−λ·dt)` is the frame-rate-independent form of a lerp: the same visual rate whether the
   * frame took 4 ms or 40 ms, which a bare `lerp(x, 0.1)` does not give.
   */
  update(dt: number): void {
    const step = Math.max(0, Math.min(0.25, dt));
    if (this.#mode === "free") this.#integrateFree(step);
    this.#computeDesired();
    this.keepCameraOutsideBuildings(this.#desiredPosition, this.#desiredLook);

    const kp = 1 - Math.exp(-step * this.positionLambda);
    const kl = 1 - Math.exp(-step * this.lookLambda);
    const kf = 1 - Math.exp(-step * this.fovLambda);
    this.camera.position.lerp(this.#desiredPosition, kp);
    this.look.lerp(this.#desiredLook, kl);

    // The smoothed position can still clip a roof on the way down; fix it after the lerp too.
    this.keepCameraOutsideBuildings(this.camera.position, this.look);

    const fov = this.camera.fov + (this.#desiredFov - this.camera.fov) * kf;
    if (Math.abs(fov - this.camera.fov) > 1e-4) {
      this.camera.fov = fov;
      this.camera.updateProjectionMatrix();
    }
    this.camera.lookAt(this.look);
    this.camera.updateMatrixWorld();
  }

  /**
   * jevpilot's `keepCameraOutsideBuildings` rule (09-ui §3), in two parts:
   *
   * 1. if the camera is inside a building footprint below its roof, lift it to the roof plus a
   *    clearance. This applies at any distance, which is what keeps a fly-down from ending up in a
   *    stairwell;
   * 2. if the camera is within {@link CameraControllerOptions.occlusionRangeM} of the look target —
   *    chase and dashboard working distances — march from the target towards it and stop at the
   *    first point inside a building, so a wall never comes between the camera and the car.
   *
   * The march is deliberately *not* run at long range. During a map-to-chase fly-down the camera is
   * hundreds of metres up and the ray to the vehicle passes through every rooftop in the block; a
   * naive march would yank the camera to the vehicle's bumper on the first frame and destroy the
   * continuity the whole interaction is built on.
   *
   * Mutates `pos` in place. Returns true when it moved it.
   */
  keepCameraOutsideBuildings(pos: Vector3, lookAt: Vector3): boolean {
    const world = this.#world;
    if (!world || this.#mode === "map") return false;
    let moved = false;

    // `#liftAboveRoof` is a method rather than a closure over `pos`/`moved`: this runs twice a
    // frame, and a fresh arrow function each time is the second-largest per-frame allocation in
    // the render loop (finding Q17).
    if (this.#liftAboveRoof(pos)) moved = true;

    const dir = this.#scratchB.copy(pos).sub(lookAt);
    const dist = dir.length();
    if (dist < 1e-3 || dist > this.#occlusionRange) return moved;
    dir.multiplyScalar(1 / dist);

    const steps = Math.min(24, Math.max(4, Math.ceil(dist / 2)));
    let hitT = -1;
    for (let i = 1; i <= steps; i++) {
      const t = (dist * i) / steps;
      const x = lookAt.x + dir.x * t;
      const y = lookAt.y + dir.y * t;
      const z = lookAt.z + dir.z * t;
      const top = world.buildingTopAt(x, y);
      if (top > -Infinity && z < top) {
        hitT = t;
        break;
      }
    }
    if (hitT > 0) {
      const pull = Math.max(0.5, hitT - dist / steps);
      pos.set(lookAt.x + dir.x * pull, lookAt.y + dir.y * pull, lookAt.z + dir.z * pull);
      moved = true;
      this.#liftAboveRoof(pos);
    }
    return moved;
  }

  /** Raise `pos` to the roof plus the clearance if it is inside a building. Returns true if moved. */
  #liftAboveRoof(pos: Vector3): boolean {
    const world = this.#world;
    if (!world) return false;
    const top = world.buildingTopAt(pos.x, pos.y);
    if (top > -Infinity && pos.z < top + this.#clearance) {
      pos.z = top + this.#clearance;
      return true;
    }
    return false;
  }

  #integrateFree(dt: number): void {
    const v = this.#freeVelocity;
    if (v.lengthSq() < 1e-8) return;
    const cy = Math.cos(this.#freeYaw);
    const sy = Math.sin(this.#freeYaw);
    const cp = Math.cos(this.#freePitch);
    const sp = Math.sin(this.#freePitch);
    const fx = cy * cp;
    const fy = sy * cp;
    const fz = sp;
    // Right = forward × up, with up = +z.
    const rx = fy;
    const ry = -fx;
    this.#desiredPosition.x += (fx * v.x + rx * v.y) * dt;
    this.#desiredPosition.y += (fy * v.x + ry * v.y) * dt;
    this.#desiredPosition.z += (fz * v.x + v.z) * dt;
  }

  #computeDesired(): void {
    const d = this.#desiredPosition;
    const l = this.#desiredLook;
    switch (this.#mode) {
      case "map": {
        // Nearly straight down; see the header note on `lookAt` degeneracy.
        const horizontal = Math.max(this.#altitude * 1e-4, 1e-3);
        d.set(
          this.target.x - Math.cos(this.#bearing) * horizontal,
          this.target.y - Math.sin(this.#bearing) * horizontal,
          this.target.z + this.#altitude,
        );
        l.copy(this.target);
        break;
      }
      case "chase": {
        const base = this.#followValid ? this.#followPos : this.target;
        const yaw = this.#followHeading + Math.PI + this.#orbitYaw;
        // A little extra trail at speed; 0 at rest, +40 % at 30 m/s.
        const dist = this.#chaseDistance * (1 + Math.min(0.4, this.#followSpeed / 75));
        const cp = Math.cos(this.#orbitPitch);
        const sp = Math.sin(this.#orbitPitch);
        d.set(
          base.x + Math.cos(yaw) * dist * cp,
          base.y + Math.sin(yaw) * dist * cp,
          base.z + dist * sp + this.#chaseLookHeight,
        );
        l.set(base.x, base.y, base.z + this.#chaseLookHeight);
        break;
      }
      case "dashboard": {
        const base = this.#followValid ? this.#followPos : this.target;
        const h = this.#followHeading + this.#orbitYaw;
        const fx = Math.cos(h);
        const fy = Math.sin(h);
        d.set(base.x + fx * 0.6, base.y + fy * 0.6, base.z + this.#eyeHeight);
        const pitch = MathUtils.clamp(this.#orbitPitch - 16 * DEG, -0.5, 0.5);
        l.set(d.x + fx * 30, d.y + fy * 30, d.z + Math.tan(pitch) * 30);
        break;
      }
      case "rsu": {
        const world = this.#world;
        if (world && this.#rsuIndex >= 0 && this.#rsuIndex < world.siteCount) {
          const p = world.sitePositions;
          const i = this.#rsuIndex * 3;
          d.set(p[i], p[i + 1], p[i + 2] + 1.2);
        } else {
          d.set(this.target.x, this.target.y, this.target.z + 25);
        }
        if (this.#followValid) l.copy(this.#followPos);
        else l.set(this.target.x, this.target.y, this.target.z);
        if (l.distanceToSquared(d) < 1) l.set(d.x + 30, d.y, d.z - 8);
        break;
      }
      case "free":
      default: {
        const cy = Math.cos(this.#freeYaw);
        const sy = Math.sin(this.#freeYaw);
        const cp = Math.cos(this.#freePitch);
        l.set(
          d.x + cy * cp * 40,
          d.y + sy * cp * 40,
          d.z + Math.sin(this.#freePitch) * 40,
        );
        break;
      }
    }
  }

  /** A plain snapshot, shaped for §6.7 `view.camera`. */
  state(): CameraState {
    return {
      mode: this.#mode,
      position: { x: this.camera.position.x, y: this.camera.position.y, z: this.camera.position.z },
      target: { x: this.look.x, y: this.look.y, z: this.look.z },
      fovDeg: this.camera.fov,
      followActorId: this.#followActorId,
      altitudeM: this.#altitude,
      distanceM: this.#chaseDistance,
      bearingRad: this.#bearing,
    };
  }

  /**
   * Bind pointer and wheel input: drag to pan (map) or orbit (chase, dashboard, free, rsu), wheel to
   * change altitude or distance, shift-drag to rotate the map bearing. Returns a detach function;
   * calling {@link attachInput} again detaches the previous binding first.
   */
  attachInput(element: InputTarget): () => void {
    this.detachInput();
    let dragging = false;
    let lastX = 0;
    let lastY = 0;
    let shift = false;

    const onPointerDown = (e: Event): void => {
      const ev = e as PointerEvent;
      dragging = true;
      lastX = ev.clientX;
      lastY = ev.clientY;
      shift = ev.shiftKey || ev.button === 2;
      // Capture the pointer so a drag that leaves the canvas keeps delivering `pointermove` here
      // instead of silently ending mid-gesture.
      const target = element as { setPointerCapture?: (id: number) => void };
      if (typeof target.setPointerCapture === "function" && ev.pointerId !== undefined) {
        try {
          target.setPointerCapture(ev.pointerId);
        } catch {
          // A synthetic or already-released pointer; dragging still works without capture.
        }
      }
    };
    const onPointerMove = (e: Event): void => {
      if (!dragging) return;
      const ev = e as PointerEvent;
      const dx = ev.clientX - lastX;
      const dy = ev.clientY - lastY;
      lastX = ev.clientX;
      lastY = ev.clientY;
      if (this.#mode === "map") {
        if (shift) {
          this.#bearing -= dx * 0.005;
        } else {
          // Screen pixels → metres at the focus plane.
          const mPerPx = (2 * this.#altitude * Math.tan((this.camera.fov * DEG) / 2)) / this.#viewportH;
          const cb = Math.cos(this.#bearing);
          const sb = Math.sin(this.#bearing);
          // Screen right is (sin b, −cos b); screen up is (cos b, sin b).
          this.target.x -= (dx * sb + dy * cb) * mPerPx;
          this.target.y -= (-dx * cb + dy * sb) * mPerPx;
        }
      } else if (this.#mode === "free") {
        this.#freeYaw -= dx * 0.004;
        this.#freePitch = MathUtils.clamp(this.#freePitch - dy * 0.004, -1.5, 1.5);
      } else {
        this.#orbitYaw -= dx * 0.006;
        this.#orbitPitch = MathUtils.clamp(this.#orbitPitch + dy * 0.004, -0.35, 1.25);
      }
    };
    const onPointerUp = (_e: Event): void => {
      dragging = false;
    };
    const onWheel = (e: Event): void => {
      const ev = e as WheelEvent;
      // The gesture zooms the camera, so it must not also scroll an ancestor. That is only
      // possible on a listener registered non-passively (finding Q20): the Studio happens to set
      // `body { overflow: hidden }`, but `attachInput` is a library surface and cannot assume it.
      if (typeof ev.preventDefault === "function" && ev.cancelable !== false) ev.preventDefault();
      const k = Math.exp(ev.deltaY * 0.0012);
      if (this.#mode === "map") this.altitudeM = this.#altitude * k;
      else this.distanceM = this.#chaseDistance * k;
    };

    // `pointerleave` is deliberately *not* bound: with pointer capture a drag survives leaving the
    // element, and treating leave as pointer-up would end it mid-gesture.
    const passive: readonly (readonly [string, (e: Event) => void])[] = [
      ["pointerdown", onPointerDown],
      ["pointermove", onPointerMove],
      ["pointerup", onPointerUp],
      ["pointercancel", onPointerUp],
    ];
    for (const [type, fn] of passive) element.addEventListener(type, fn, { passive: true });
    element.addEventListener("wheel", onWheel, { passive: false });

    this.#detach = (): void => {
      for (const [type, fn] of passive) element.removeEventListener(type, fn);
      element.removeEventListener("wheel", onWheel);
    };
    return this.#detach;
  }

  /** Undo {@link attachInput}. */
  detachInput(): void {
    if (this.#detach) {
      this.#detach();
      this.#detach = null;
    }
  }

  /** Release listeners. The camera itself belongs to the caller. */
  dispose(): void {
    this.detachInput();
    this.#world = null;
  }
}
