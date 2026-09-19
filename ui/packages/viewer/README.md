# `@vwp/viewer`

The Three.js half of the V2X World Simulator UI: one scene, two views, instanced actors, overlays
and cameras. Framework-free — `apps/studio` (React) wraps it, and nothing here imports React.

Design: `docs/design/09-ui.md` §3 (one scene, two views), §4 (rendering plan and performance budget),
§5 (HUD). Data: `docs/protocol/vwp-v1.md` §3.2 (pose quantisation), §3.3/§3.4 (keyframes and deltas),
§4 (`vwp-world/1`), §6.7 (`view.*`, `overlay.set`).

## Conventions

- **Coordinates are the protocol's.** ENU metres, `x` east, `y` north, **`z` up** — the same numbers
  `PoseBuffer.positions` and the world payload carry, written straight into instance matrices with no
  transform. Every camera and light this package creates gets `up = (0, 0, 1)`;
  `THREE.Object3D.DEFAULT_UP` is never mutated.
- **Heading is radians counter-clockwise from +x**, matching the engine's `atan2(dy, dx)` (§3.2), so an
  actor's instance matrix is a yaw about +z and a translation, nothing more.
- **Colours** are declared as packed sRGB hex in `theme.ts` and converted once; the four actor states
  use the Okabe–Ito colour-blind-safe palette with redundant marker *shapes* (09-ui §10). "Colour-blind
  safe" is a measured claim, not a provenance claim: `test/theme.test.ts` simulates the three
  dichromacies and requires ΔE00 ≥ 15 between the state colours and 3:1 contrast against the
  background, and `ActorRenderer.legend()` reports exactly the colours the scene writes into
  `instanceColor`, so a DOM legend built from it cannot drift from the render.
- **Time is the engine's.** Poses are interpolated on `simTimeNs`, never on the wall-clock instant a
  frame arrived; arrival time is used only to notice that the stream has gone quiet.

## Quick start

```ts
import { Viewer } from "@vwp/viewer";
import { VwpClient, decodeWorld } from "@vwp/protocol";

const viewer = new Viewer({ canvas, theme: "dark", timeOfDay: 11 });
const client = new VwpClient({ url: "ws://127.0.0.1:8787" });
viewer.attachClient(client);                 // Hello → classes, Keyframe → signals, both → poses

const hello = await client.connect();
const res = await fetch(`/world/${bytesToHex(hello.worldHash)}.vwb`);
viewer.setWorld(decodeWorld(await res.arrayBuffer()));

canvas.addEventListener("click", (e) => viewer.clickAtPixel(e.offsetX, e.offsetY, "chase"));
viewer.cameras.attachInput(canvas);
viewer.overlays.set("tx_pulses", true);
```

## What is in here

| Module | What it owns |
|---|---|
| `scene.ts` | `Viewer`: the one `Scene`, `WebGLRenderer` and `PerspectiveCamera`, and the rAF loop |
| `world-render.ts` | `WorldRenderer`: merged per-tile roads/markings/junctions/landuse, buildings in one `BatchedMesh` with three LODs, signals, RSU masts, ground, sky, sun |
| `actors.ts` | `ActorRenderer`: one `InstancedMesh` per (class × LOD), CPU frustum culling by `count`, per-instance state colour |
| `interp.ts` | `PoseInterpolator`: two snapshots, smooth motion at 60 fps from any delta cadence, keyed on sim time, with an extrapolation ease |
| `cameras.ts` | `CameraController`: `map`/`chase`/`dashboard`/`free`/`rsu`, exponential smoothing, `flyTo`, `keepCameraOutsideBuildings` |
| `overlays.ts` | `OverlayManager` and the five overlays, keyed by `OVERLAY_NAMES` from the protocol, with the `GT` lock; the non-GT state-marker channels start enabled, because a shape channel that has to be switched on is not redundancy |
| `picking.ts` | `Picker`: grid broad phase + exact ray/OBB narrow phase over actors, plus sites and the ground plane |
| `stats.ts` | `FrameStats`: frame time, CPU/render split, draw calls, instance counts |
| `geometry.ts` | `MeshBuilder` and the procedural vehicle, building, ring and marker geometry |
| `theme.ts` | Dark and light palettes |

## One scene, two views

There is exactly one camera. `map` is a camera mode, not a separate renderer, canvas or scene graph,
so `viewer.flyTo(actorId, "chase")` from the top-down view is a continuous camera path: the mode change
re-aims the *desired* pose and `1 − exp(−4·dt)` (position) / `1 − exp(−6·dt)` (look target) carries the
camera down. The headless test measures this: 700 m → 4.2 m with a largest single-frame move of 53 m
against an 834 m initial gap, and no cut.

Top-down is 10 cm off vertical (`altitude · 1e-4`) because `Object3D.lookAt` is degenerate when the view
direction is parallel to `up`, which for a z-up world is exactly the 90° pitch the design asks for.

## The pose clock

A snapshot is dated by `PoseBuffer.simTimeNs` (§3.3.1/§3.4.1). `capture()` also records when it
arrived, but only to notice silence: everything that positions an actor comes from sim time, because
the engine is forbidden a wall clock (ADR 0004) and a viewer that re-introduces one turns network
jitter straight into motion.

`sample()` therefore maintains a line from the viewer clock to sim time — a minimum-delay filter over
arrivals, plus a rate estimate over a multi-second baseline that *is* `run.speed` — and advances its
render clock at that rate ±10 %, never taking the estimate's value outright. Every window (render
delay, extrapolation budget, stall) is a multiple of the **measured snapshot interval**, so a 2 s
cadence (0.05x playback, a keyframe-only run, a congested link) interpolates exactly like a 0.1 s one.

Outside the segment the two snapshots span, the clock glides to a stop over `maxExtrapolationSteps`
intervals instead of being clipped, in both directions, and never reverses. Measured at 60 fps
(`test/interp-timing.test.ts`), per-frame motion of one actor:

| | per-frame motion | frozen frames |
|---|---|---|
| 0.1 s cadence, ±40 ms arrival jitter | 0.1755–0.2239 m | 0 % |
| 0.1 s / 0.2 / 0.35 / 0.5 / 1.0 / 2.0 s cadence | 0.1995–0.2200 m | 0 % |
| 0.5–2.0 s cadence with ±10 % jitter | 0.1768–0.2231 m | 0 % |
| crossing the stall boundary | no backwards step; ≤ 32 % deceleration per frame | — |

## Performance

Measured on this machine (Apple silicon, Node 24.7, `pnpm --filter @vwp/viewer bench`), with the GPU
stubbed so the numbers are the **CPU half** of the frame:

| | |
|---|---|
| 5,000 vehicles, all inside the frustum, 600 frames | mean **0.78 ms/frame** wall, p95 1.74 ms, p99 1.93 ms |
| viewer CPU (interpolate + cull + LOD + instance write) | mean **0.56 ms** |
| draw calls, whole scene | **113** (103 static + 8 actor buckets + 2 state-marker channels; 675 buildings are 1) |
| heap delta over 600 frames, forced GC | **0.07 MB** |
| CPU frustum culling, 450 m map view | 418 drawn of 5,000 live |

09-ui §4 budgets "1–2 ms in JS" for pose sampling and matrix writes at 5,000 vehicles; the measurement
above is inside it. What these numbers do **not** cover is rasterisation, shader compilation or GPU
upload, which Node cannot measure — the 60 fps claim still needs a browser on an integrated GPU.

Rules the render loop keeps, all asserted in `test/render-perf.test.ts`: no per-frame allocation
(−29 B/frame at 2,000 actors under a forced GC, and zero `addUpdateRange` calls — three's
implementation pushes a fresh `{start, count}` per call, so each attribute owns one range object);
no per-frame `Object3D` or `Matrix4`; instance matrices written directly into `instanceMatrix.array`;
instance **colours** uploaded only when a selection or a §3.3.4 state bit actually changes (0 uploads
and 0 bytes over 60 frames of a frozen pose buffer); pose clearing proportional to the live actor
count rather than to `Hello.actor_capacity`; and instance buckets that stop growing after warm-up.

## Tests

```sh
pnpm --filter @vwp/viewer typecheck
pnpm --filter @vwp/viewer test
pnpm --filter @vwp/viewer bench     # the budget test with --expose-gc
```

The tests run headless: `test/support/null-renderer.ts` replaces the GPU but still walks the scene and
counts the drawables a real renderer would submit, and `test/support/fixture.ts` builds its world with
`encodeWorld`/`decodeWorld` and its poses with `keyframeFrame`/`deltaFrame` → `PoseBuffer`, so the
viewer is fed bytes a conforming server could have sent.
