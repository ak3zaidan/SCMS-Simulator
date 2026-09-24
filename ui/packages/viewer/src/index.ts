/**
 * `@vwp/viewer` — the Three.js half of the V2X World Simulator UI.
 *
 * Framework-free by design (09-ui §2): the Studio is React, this is not, and the only contract
 * between them is the API below. Everything is in ENU metres with `z` up, exactly as
 * `vwp-world/1` (§4) and the pose buffer (§3.2) deliver it.
 *
 * ```ts
 * import { Viewer } from "@vwp/viewer";
 * import { VwpClient, decodeWorld } from "@vwp/protocol";
 *
 * const viewer = new Viewer({ canvas, theme: "dark", timeOfDay: 11 });
 * const client = new VwpClient({ url: "ws://127.0.0.1:8787" });
 * viewer.attachClient(client);
 * const hello = await client.connect();
 * viewer.setWorld(decodeWorld(await fetchWorld(hello.worldHash)));
 *
 * canvas.addEventListener("click", (e) => viewer.clickAtPixel(e.offsetX, e.offsetY, "chase"));
 * viewer.overlays.set("tx_pulses", true);
 * ```
 */

export { Viewer } from "./scene.js";
export type { ViewerOptions, FrameReport, ActorFraming } from "./scene.js";

export { WorldRenderer, pointInRing } from "./world-render.js";
export type { WorldRendererOptions, WorldBuildReport, BuildingBackend } from "./world-render.js";

export {
  ActorRenderer, classesFromHello, DEFAULT_ACTOR_CLASSES, ACTOR_STATE_COLOR_KEYS, actorColorKey,
  actorStateColorIndex,
} from "./actors.js";
export type {
  ActorRendererOptions, ActorUpdateContext, ActorUpdateStats, ActorLegendEntry,
} from "./actors.js";

export { PoseInterpolator, lerpAngle, wrapAngle, extrapolationEase, HISTORY as POSE_HISTORY } from "./interp.js";
export type { PoseInterpolatorOptions, PoseSnapshot, SampleInfo } from "./interp.js";

export { CameraController, CAMERA_MODES } from "./cameras.js";
export type { CameraControllerOptions, CameraState, CameraMode, InputTarget } from "./cameras.js";

export {
  OverlayManager, TxPulseOverlay, LinkOverlay, HeatmapOverlay, CoverageOverlay, StateMarkerOverlay,
  ActorLocatorOverlay, GROUND_TRUTH_OVERLAYS, isGroundTruthOverlay, overlayLabel, VRU_MARK_SCALE,
} from "./overlays.js";
export type {
  OverlayManagerOptions, OverlayEntry, OverlayUpdateContext, MarkerChannel,
} from "./overlays.js";

export { Picker } from "./picking.js";
export type { PickerOptions, PickerPoses } from "./picking.js";

export { FrameStats } from "./stats.js";
export type { FrameStatsSnapshot, FrameCounters } from "./stats.js";

export { DARK_THEME, LIGHT_THEME, themeByName } from "./theme.js";
export type { ViewerTheme, ActorStateColorKey } from "./theme.js";

export {
  MeshBuilder, addRibbon, addPolygon, addDisc, addBox, addCylinder, addExtrudedRing, earClip,
  ringSignedArea, buildActorGeometry, discGeometry, ringGeometry, markerGeometry,
  withUnitVertexColors, isRiddenVru,
} from "./geometry.js";
export type { MeshBuilderOptions, MarkerShape, RingShading } from "./geometry.js";

export { ENU_UP, LOD_LEVELS, VEHICLE_MARK_ANGULAR_RADIUS, vecToPlain } from "./types.js";
export type {
  ActorClassDef, LodLevel, PickResult, Vec3Like, ViewerCanvas, ViewerRenderer, RendererInfoLike,
  FrameScheduler,
} from "./types.js";

/** The protocol version this viewer renders. */
export const VWP_VIEWER_VERSION = "0.1.0";
