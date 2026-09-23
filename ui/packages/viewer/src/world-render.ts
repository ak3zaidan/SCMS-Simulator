/**
 * Static world construction from a decoded `vwp-world/1` payload (docs/protocol/vwp-v1.md §4).
 *
 * 09-ui §4 asks for "merged static geometry per tile … unzoomed = one drawable per category, zoomed =
 * per-tile drawables with quadtree culling" for roads, and `BatchedMesh` with three LODs for
 * buildings. What is built here:
 *
 * - **Roads, junctions, crossings, landuse** are merged into **one vertex-coloured mesh per tile**
 *   rather than one per tile *per category*. A 2 km world at the default 300 m tile is 7×7 tiles, so
 *   the whole ground plan is ~49 draw calls instead of ~245, and three's own frustum culling drops
 *   the off-screen ones. Category is carried in the vertex colour, which costs three floats a vertex
 *   and saves a material switch.
 * - **Lane markings** are a second mesh per tile (unlit, polygon-offset) so `overlay.set
 *   {lane_markings: false}` is one `visible = false` per tile and nothing else.
 * - **Buildings** go into a single `THREE.BatchedMesh`, one geometry per building: its real
 *   footprint extruded, with a roof and a parapet. There used to be three LODs, and the far one was
 *   the footprint's *axis-aligned bounding box* — on Manhattan's grid, which runs 29° off north, a
 *   box that swallows half of every street around the block, so from 700 m out the traffic drove
 *   through solid walls. The near/mid switch popped the parapet at 180 m. One exact geometry
 *   costs fewer vertices than the three did together and cannot pop. If `BatchedMesh` cannot be
 *   built — the vertex budget is exceeded, or a three build without multi-draw support throws —
 *   the builder falls back to baking the shells into the per-tile meshes and reports
 *   {@link WorldRenderer.buildingBackend} as `"merged"`.
 *
 * Also here: the ground plane, the gradient sky, and a sun/hemisphere rig driven by a time-of-day
 * parameter, plus the building occupancy grid that `cameras.ts` uses for `keepCameraOutsideBuildings`.
 */

import {
  BackSide,
  BatchedMesh,
  BufferGeometry,
  Color,
  DirectionalLight,
  DoubleSide,
  Group,
  HemisphereLight,
  InstancedMesh,
  Matrix4,
  Mesh,
  MeshBasicMaterial,
  MeshLambertMaterial,
  PlaneGeometry,
  ShaderMaterial,
  SphereGeometry,
  Vector3,
  type Camera,
  type Material,
} from "three";
import type { SignalBlock, VwpWorld } from "@vwp/protocol";
import { SignalRenderer } from "./signals.js";
import { MeshBuilder, addBox, addCylinder, addDisc, addExtrudedRing, addPolygon, addRibbon } from "./geometry.js";
import type { RingShading } from "./geometry.js";
import type { ViewerTheme } from "./theme.js";
import type { LodLevel } from "./types.js";

/** §4.3 lane types. */
const LANE_DRIVE = 0;
const LANE_BIKE = 1;
const LANE_SIDEWALK = 2;
const LANE_BUS = 3;
const LANE_PARKING = 4;
const LANE_JUNCTION_INTERNAL = 5;
const LANE_CROSSING = 6;

/** Height offsets, metres above the terrain, chosen to stay clear of depth-buffer noise at 500 m. */
const Z_LANDUSE = 0.02;
const Z_ROAD = 0.1;
const Z_JUNCTION = 0.16;
const Z_CROSSING = 0.2;
const Z_MARKING = 0.24;

/** Tuning for {@link WorldRenderer}. */
export interface WorldRendererOptions {
  readonly theme: ViewerTheme;
  /** Tile edge in metres for the merged road meshes. Default 300. */
  readonly tileSizeM?: number;
  /** Build buildings at all. Default true. */
  readonly buildings?: boolean;
  /** Build lane markings. Default true. */
  readonly laneMarkings?: boolean;
  /**
   * `[near→mid, mid→far]` building LOD switch distances in metres. Default `[180, 700]`. Kept for
   * compatibility: every LOD is now the same exact footprint geometry (see the file header), so
   * these only decide which id is recorded, never what is drawn.
   */
  readonly buildingLodDistancesM?: readonly [number, number];
  /** Refuse `BatchedMesh` above this many vertices and fall back to merged tiles. Default 1,500,000. */
  readonly maxBuildingVertices?: number;
  /** Cast and receive shadows. Default true. */
  readonly shadows?: boolean;
  /** Half-edge of the sun's shadow frustum in metres. Default 260. */
  readonly shadowExtentM?: number;
  /** Shadow map edge in texels. Default 2048 (09-ui §4). */
  readonly shadowMapSize?: number;
  /**
   * How dark a cast shadow gets, `[0, 1]`. Default 0.55.
   *
   * A Midtown street at 11:00 is *correctly* in the shadow of its own buildings — that shadowing is
   * the physics the simulator models, so hiding it would be wrong. At full strength, though, a
   * shadowed road received only the sky and the fill, about a quarter of the sunlit irradiance, and
   * landed near #1f1f1f on a #0b0f14 background: the street was legible only where the sun happened
   * to reach down it. Compressing the shadow keeps it plainly visible as a shadow and keeps the
   * surface under it readable, which is the trade an instrument should make.
   */
  readonly shadowStrength?: number;
  /** Hours in `[0, 24)`. Default 11. */
  readonly timeOfDay?: number;
  /**
   * How much of a wall's colour is removed at its foot, `[0, 1)`. Default 0.45.
   *
   * See {@link RingShading}: a directional sun 57° up gives a vertical wall almost nothing, so
   * without these three terms a city of prisms renders as one flat silhouette. They are baked into
   * the vertex colours and cost nothing per frame.
   */
  readonly buildingBaseOcclusion?: number;
  /** Value swing applied by wall azimuth, `[0, 1)`. Default 0.2. */
  readonly buildingFaceRelief?: number;
  /** Peak per-building tone deviation, `[0, 1)`. Default 0.11. */
  readonly buildingToneVariation?: number;
  /**
   * Dim light from the anti-sun azimuth, as a fraction of the sun's intensity. Default 0.3.
   *
   * A single directional light plus a hemisphere fill leaves every wall facing away from the sun at
   * the hemisphere term alone, which is the same value for all of them: adjacent buildings merge.
   * This is the bounce off the opposite façade, and it is what separates them. It never casts.
   */
  readonly fillLightFraction?: number;
}

/** Which building path was taken. */
export type BuildingBackend = "batched" | "merged" | "none";

/** Bookkeeping about a built world, for the HUD and for tests. */
export interface WorldBuildReport {
  readonly tiles: number;
  readonly lanes: number;
  readonly buildings: number;
  readonly junctions: number;
  readonly signals: number;
  readonly sites: number;
  readonly crossings: number;
  readonly landuse: number;
  readonly buildingBackend: BuildingBackend;
  readonly buildingVertices: number;
  readonly buildingIndices: number;
  readonly surfaceVertices: number;
  readonly markingVertices: number;
  readonly drawables: number;
  readonly buildMs: number;
}

/**
 * How far up-sun of its target the `DirectionalLight` is placed, metres.
 *
 * Only the direction matters to the shading; this is chosen to sit comfortably inside the light's
 * own `shadow.camera.far` of 2,500 m with the whole city in front of it.
 */
const SUN_DISTANCE_M = 900;

const SUN_NOON = new Color(0xfff4e4);
const SUN_LOW = new Color(0xff9a52);
const NIGHT_TOP = new Color(0x05070f);
const DUSK_HORIZON = new Color(0xff8a4a);
const NIGHT_HORIZON = new Color(0x0a1020);

const SKY_VERTEX = /* glsl */ `
varying vec3 vWorld;
void main() {
  vWorld = (modelMatrix * vec4(position, 1.0)).xyz - cameraPosition;
  gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0);
}
`;

const SKY_FRAGMENT = /* glsl */ `
uniform vec3 uTop;
uniform vec3 uHorizon;
uniform vec3 uBottom;
uniform vec3 uSun;
uniform float uSunIntensity;
varying vec3 vWorld;
void main() {
  vec3 dir = normalize(vWorld);
  float h = clamp(dir.z, -1.0, 1.0);
  // pow(h, 0.35) rather than 0.55: the gradient then spends most of its range in the first few
  // degrees above the horizon, which is the only part of the sky a street-level camera sees. With
  // the flatter curve the whole upper hemisphere was one value and there was no horizon at all.
  vec3 sky = h >= 0.0 ? mix(uHorizon, uTop, pow(h, 0.35)) : mix(uHorizon, uBottom, pow(-h, 0.7));
  // A narrow brightening either side of h = 0. This is the line the buildings are read against;
  // without it the roofline has nothing to be a silhouette of.
  sky += uHorizon * 0.42 * exp(-abs(h) * 26.0);
  float sun = max(dot(dir, normalize(uSun)), 0.0);
  sky += uSunIntensity * vec3(1.0, 0.86, 0.66) * (pow(sun, 620.0) * 1.6 + pow(sun, 12.0) * 0.16);
  gl_FragColor = vec4(sky, 1.0);
}
`;

/**
 * Builds and owns the static half of the scene.
 *
 * Add {@link group} to the scene once; call {@link setWorld} whenever a new `vwp-world/1` payload
 * arrives, {@link updateSignals} on every keyframe, and {@link updateLod} once per frame.
 */
export class WorldRenderer {
  /** Everything static, in ENU metres. */
  readonly group = new Group();
  readonly tiles = new Group();
  readonly markings = new Group();
  readonly buildingsGroup = new Group();
  /** Signal lanterns and stop bars; see `signals.ts`. */
  readonly signals: SignalRenderer;
  readonly signalsGroup: Group;
  readonly sitesGroup = new Group();
  readonly lights = new Group();

  readonly sun: DirectionalLight;
  /** Dim, shadowless bounce from the anti-sun side; see {@link WorldRendererOptions.fillLightFraction}. */
  readonly fill: DirectionalLight;
  readonly hemisphere: HemisphereLight;
  readonly sky: Mesh<SphereGeometry, ShaderMaterial>;
  readonly ground: Mesh<PlaneGeometry, MeshLambertMaterial>;

  #theme: ViewerTheme;
  #options: Required<Omit<WorldRendererOptions, "theme">>;
  #world: VwpWorld | null = null;
  #report: WorldBuildReport = {
    tiles: 0, lanes: 0, buildings: 0, junctions: 0, signals: 0, sites: 0, crossings: 0, landuse: 0,
    buildingBackend: "none", buildingVertices: 0, buildingIndices: 0, surfaceVertices: 0,
    markingVertices: 0, drawables: 0, buildMs: 0,
  };

  #surfaceMaterial: MeshLambertMaterial;
  #markingMaterial: MeshBasicMaterial;
  #buildingMaterial: MeshLambertMaterial;
  #siteMaterial: MeshLambertMaterial;
  #disposables: (BufferGeometry | Material)[] = [];

  #buildings: BatchedMesh | null = null;
  /** `3·b + lod` → BatchedMesh geometry id. */
  #buildingGeomIds = new Int32Array(0);
  /** Building index → BatchedMesh instance id. */
  #buildingInstanceIds = new Int32Array(0);
  /** Building index → currently selected LOD. */
  #buildingLod = new Uint8Array(0);
  /** Building centroid x, y, top z, and footprint radius. */
  #buildingCentroid = new Float32Array(0);
  #buildingCount = 0;
  /** Buildings hidden this frame because the followed vehicle is inside them; see {@link setGhostBuilding}. */
  #ghost = -1;
  #buildingBackend: BuildingBackend = "none";
  #buildError: string | null = null;

  /** Occupancy grid over building footprints, CSR: `cellStart`, `cellItems`. */
  #gridMinX = 0;
  #gridMinY = 0;
  #gridCell = 30;
  #gridW = 0;
  #gridH = 0;
  #gridStart = new Int32Array(0);
  #gridItems = new Int32Array(0);


  /** Site positions, three floats each, at antenna height. */
  #sitePos = new Float32Array(0);
  #siteIds = new Uint32Array(0);
  #siteNodeIds = new Uint32Array(0);
  #siteKinds = new Uint8Array(0);
  #siteCount = 0;

  #timeOfDay = 11;
  #lodCamPos = new Vector3(NaN, NaN, NaN);
  #scratchMatrix = new Matrix4();
  #scratchColor = new Color();
  #sunDir = new Vector3(0, 0, 1);
  #buildingsVisible = 0;

  constructor(options: WorldRendererOptions) {
    this.#theme = options.theme;
    this.#options = {
      tileSizeM: options.tileSizeM ?? 300,
      buildings: options.buildings ?? true,
      laneMarkings: options.laneMarkings ?? true,
      buildingLodDistancesM: options.buildingLodDistancesM ?? [180, 700],
      maxBuildingVertices: options.maxBuildingVertices ?? 1_500_000,
      shadows: options.shadows ?? true,
      shadowExtentM: options.shadowExtentM ?? 260,
      shadowMapSize: options.shadowMapSize ?? 2048,
      shadowStrength: options.shadowStrength ?? 0.55,
      timeOfDay: options.timeOfDay ?? 11,
      buildingBaseOcclusion: options.buildingBaseOcclusion ?? 0.45,
      buildingFaceRelief: options.buildingFaceRelief ?? 0.2,
      buildingToneVariation: options.buildingToneVariation ?? 0.11,
      fillLightFraction: options.fillLightFraction ?? 0.3,
    };
    this.#timeOfDay = this.#options.timeOfDay;

    this.group.name = "world";
    this.tiles.name = "world/tiles";
    this.markings.name = "world/lane-markings";
    this.buildingsGroup.name = "world/buildings";
    this.signals = new SignalRenderer(options.theme);
    this.signalsGroup = this.signals.group;
    this.sitesGroup.name = "world/sites";
    this.lights.name = "world/lights";

    this.#surfaceMaterial = new MeshLambertMaterial({ vertexColors: true, name: "world-surface" });
    this.#markingMaterial = new MeshBasicMaterial({
      vertexColors: true, name: "lane-markings", polygonOffset: true, polygonOffsetFactor: -2, polygonOffsetUnits: -2,
      toneMapped: false,
    });
    this.#buildingMaterial = new MeshLambertMaterial({ vertexColors: true, name: "buildings" });
    this.#siteMaterial = new MeshLambertMaterial({ vertexColors: true, name: "sites" });

    const groundGeom = new PlaneGeometry(1, 1);
    this.ground = new Mesh(groundGeom, new MeshLambertMaterial({ name: "ground", side: DoubleSide }));
    this.ground.name = "world/ground";
    this.ground.receiveShadow = this.#options.shadows;
    this.ground.matrixAutoUpdate = false;

    const skyGeom = new SphereGeometry(1, 24, 16);
    this.sky = new Mesh(skyGeom, new ShaderMaterial({
      name: "sky",
      uniforms: {
        uTop: { value: new Color(this.#theme.skyTop) },
        uHorizon: { value: new Color(this.#theme.skyHorizon) },
        uBottom: { value: new Color(this.#theme.skyBottom) },
        uSun: { value: new Vector3(0.3, 0.3, 0.9) },
        uSunIntensity: { value: 1 },
      },
      vertexShader: SKY_VERTEX,
      fragmentShader: SKY_FRAGMENT,
      side: BackSide,
      depthWrite: false,
      depthTest: false,
      toneMapped: true,
    }));
    this.sky.name = "world/sky";
    this.sky.renderOrder = -1000;
    this.sky.frustumCulled = false;
    this.sky.matrixAutoUpdate = false;
    this.sky.scale.setScalar(6000);

    this.sun = new DirectionalLight(0xfff2e0, 2.2);
    this.sun.name = "world/sun";
    this.sun.up.set(0, 0, 1);
    this.sun.castShadow = this.#options.shadows;
    this.sun.shadow.mapSize.set(this.#options.shadowMapSize, this.#options.shadowMapSize);
    this.sun.shadow.camera.up.set(0, 0, 1);
    this.sun.shadow.camera.near = 1;
    this.sun.shadow.camera.far = 2500;
    this.sun.shadow.bias = -0.0005;
    this.sun.shadow.normalBias = 0.05;
    this.sun.shadow.intensity = Math.max(0, Math.min(1, this.#options.shadowStrength));
    this.#applyShadowExtent();
    this.lights.add(this.sun, this.sun.target);

    this.fill = new DirectionalLight(0xdfe9ff, 0.7);
    this.fill.name = "world/fill";
    this.fill.up.set(0, 0, 1);
    this.fill.castShadow = false;
    this.lights.add(this.fill, this.fill.target);

    this.hemisphere = new HemisphereLight(this.#theme.skyHorizon, this.#theme.ground, 0.9);
    this.hemisphere.name = "world/hemisphere";
    this.lights.add(this.hemisphere);

    this.group.add(this.sky, this.ground, this.tiles, this.markings, this.buildingsGroup,
      this.signalsGroup, this.sitesGroup, this.lights);
    this.setTheme(this.#theme);
    this.setTimeOfDay(this.#timeOfDay);
    this.#resizeGround(2000);
  }

  /** The world currently built, or null. */
  get world(): VwpWorld | null {
    return this.#world;
  }

  /** Bookkeeping about the last {@link setWorld}. */
  get report(): WorldBuildReport {
    return this.#report;
  }

  /** Which building path the last build took. */
  get buildingBackend(): BuildingBackend {
    return this.#buildingBackend;
  }

  /** Why `BatchedMesh` was abandoned, when it was. */
  get buildingBackendReason(): string | null {
    return this.#buildError;
  }

  /** Buildings whose LOD instance is currently visible. */
  get buildingsVisible(): number {
    return this.#buildingsVisible;
  }

  /** Infrastructure site positions at antenna height, three floats per site. */
  get sitePositions(): Float32Array {
    return this.#sitePos;
  }

  /** `site_id` per site. */
  get siteIds(): Uint32Array {
    return this.#siteIds;
  }

  /** `node_id` per site, `0xFFFFFFFF` when unassigned. */
  get siteNodeIds(): Uint32Array {
    return this.#siteNodeIds;
  }

  /** `kind` per site: 0 rsu, 1 cell, 2 other. */
  get siteKinds(): Uint8Array {
    return this.#siteKinds;
  }

  /** Number of sites in the world. */
  get siteCount(): number {
    return this.#siteCount;
  }

  /** Hours in `[0, 24)`. */
  get timeOfDay(): number {
    return this.#timeOfDay;
  }

  /**
   * Set the time of day, which drives the sun's elevation, azimuth, colour and intensity, the
   * hemisphere fill, and the sky gradient. Hours outside `[0, 24)` wrap.
   */
  setTimeOfDay(hours: number): void {
    const t = ((hours % 24) + 24) % 24;
    this.#timeOfDay = t;
    // Sun elevation: below the horizon before 06:00 and after 18:00, ~60° at noon.
    const dayFrac = (t - 6) / 12;
    const elevation = Math.sin(dayFrac * Math.PI) * (Math.PI * 0.34) - 0.035;
    const azimuth = Math.PI * 1.5 - dayFrac * Math.PI; // rises in the east, sets in the west
    const ce = Math.cos(elevation);
    const dirX = Math.cos(azimuth) * ce;
    const dirY = Math.sin(azimuth) * ce;
    const dirZ = Math.sin(elevation);
    const daylight = Math.max(0, dirZ);

    this.#sunDir.set(dirX, dirY, Math.max(0.03, dirZ)).normalize();
    this.#placeSun();
    this.sun.intensity = 0.15 + daylight * 2.6;
    // Warm near the horizon, neutral at noon.
    const warmth = 1 - Math.min(1, daylight * 2.4);
    this.sun.color.copy(SUN_NOON).lerp(SUN_LOW, warmth);
    this.sun.castShadow = this.#options.shadows && daylight > 0.05;

    // The fill comes from the opposite azimuth at a low elevation — the bounce off the façade
    // across the street. Low, because a high fill washes the vertical relief back out.
    this.fill.position.set(-dirX, -dirY, 0.34).multiplyScalar(700);
    this.fill.intensity = this.sun.intensity * this.#options.fillLightFraction;

    // The sky is the only light a shadowed surface gets besides the fill, so it carries the shade.
    this.hemisphere.intensity = 0.45 + daylight * 1.35;

    const u = this.sky.material.uniforms;
    const night = 1 - Math.min(1, daylight * 3);
    (u.uTop.value as Color).setHex(this.#theme.skyTop).lerp(NIGHT_TOP, night * 0.85);
    (u.uHorizon.value as Color).setHex(this.#theme.skyHorizon)
      .lerp(DUSK_HORIZON, Math.max(0, 1 - Math.abs(daylight - 0.08) * 8) * 0.55)
      .lerp(NIGHT_HORIZON, night * 0.7);
    (u.uBottom.value as Color).setHex(this.#theme.skyBottom);
    (u.uSun.value as Vector3).set(dirX, dirY, dirZ);
    u.uSunIntensity.value = daylight;
  }

  /** Swap the palette. Rebuilds vertex colours only if a world is loaded. */
  setTheme(theme: ViewerTheme): void {
    this.#theme = theme;
    this.ground.material.color.setHex(theme.ground);
    this.hemisphere.color.setHex(theme.skyHorizon);
    this.hemisphere.groundColor.setHex(theme.ground);
    this.signals.setTheme(theme);
    this.setTimeOfDay(this.#timeOfDay);
    if (this.#world) this.setWorld(this.#world);
  }

  /** Where the sun's shadow frustum is centred; follow the camera focus to keep texels small. */
  setShadowFocus(x: number, y: number, z: number): void {
    // Snap the focus to the shadow map's texel grid, in the light's own frame. A frustum that
    // slides by a fraction of a texel every frame re-rasterises every shadow edge at a new phase,
    // and the edges crawl and shimmer as the chase camera drives along — the classic "shadow
    // swimming" (Dimitrov 2007, "Cascaded Shadow Maps", NVIDIA, §"Moving the light texel-sized
    // increments"). Along the light direction nothing needs snapping.
    const texel = (2 * this.#options.shadowExtentM) / Math.max(1, this.#options.shadowMapSize);
    const d = this.#sunDir;
    // Light-space basis: `u` horizontal and perpendicular to the sun, `v = d × u`.
    let ux = -d.y;
    let uy = d.x;
    const ul = Math.hypot(ux, uy);
    if (ul < 1e-6) {
      ux = 1;
      uy = 0;
    } else {
      ux /= ul;
      uy /= ul;
    }
    const vx = d.y * 0 - d.z * uy;
    const vy = d.z * ux - d.x * 0;
    const vz = d.x * uy - d.y * ux;
    const pu = x * ux + y * uy;
    const pv = x * vx + y * vy + z * vz;
    const pd = x * d.x + y * d.y + z * d.z;
    const su = Math.round(pu / texel) * texel;
    const sv = Math.round(pv / texel) * texel;
    this.sun.target.position.set(
      su * ux + sv * vx + pd * d.x,
      su * uy + sv * vy + pd * d.y,
      sv * vz + pd * d.z,
    );
    this.sun.target.updateMatrixWorld();
    this.#placeSun();
  }

  /**
   * Put the sun `SUN_DISTANCE_M` up-sun of its own target.
   *
   * A `DirectionalLight` shines along `position − target.position`, and `setShadowFocus` moves the
   * target to whatever the camera is looking at. The position used to be set in absolute world
   * coordinates — `direction × 900` — which made the light's *direction* a function of where in the
   * world the camera was pointed. In a world whose ENU origin is a kilometre or two from the
   * streets, an 11:00 sun computed at 57° elevation arrived at the geometry at about 15°, so
   * horizontal surfaces got a grazing light, vertical walls got almost none, and the whole scene
   * rendered three to four times too dark — the black void the review saw. It also meant the
   * lighting shifted as the user panned. Offsetting from the target fixes the direction and keeps
   * the shadow frustum, which is sized in the light's own space, centred on the same point.
   */
  #placeSun(): void {
    const t = this.sun.target.position;
    this.sun.position.set(
      t.x + this.#sunDir.x * SUN_DISTANCE_M,
      t.y + this.#sunDir.y * SUN_DISTANCE_M,
      t.z + this.#sunDir.z * SUN_DISTANCE_M,
    );
  }

  /** The sun's unit direction, `position − target`, normalised. */
  get sunDirection(): Vector3 {
    return this.#sunDir;
  }

  #applyShadowExtent(): void {
    const e = this.#options.shadowExtentM;
    const c = this.sun.shadow.camera;
    c.left = -e;
    c.right = e;
    c.top = e;
    c.bottom = -e;
    c.updateProjectionMatrix();
  }

  #resizeGround(radiusM: number): void {
    // ×6 rather than ×4: at a 1.25 m eye height the geometric horizon is about 4 km out, and at ×4
    // a 2 km world's ground plane ended at almost exactly that distance, so the edge of the world
    // was on the skyline. It is one quad either way.
    const size = Math.max(200, radiusM * 6);
    this.ground.geometry.dispose();
    this.ground.geometry = new PlaneGeometry(size, size);
    this.ground.updateMatrix();
  }

  /**
   * Build the static scene from a decoded world. Replaces whatever was there.
   *
   * Cost is linear in lane points + ring points; the Manhattan fixture (≈ 1,900 lanes, ≈ 1,000
   * buildings) builds in a few tens of milliseconds, which is why this is done synchronously.
   */
  setWorld(world: VwpWorld): void {
    const t0 = nowMs();
    this.#clearWorld();
    this.#world = world;

    const bbox = world.bbox;
    const cx = (bbox.minXM + bbox.maxXM) / 2;
    const cy = (bbox.minYM + bbox.maxYM) / 2;
    const spanX = Math.max(1, bbox.maxXM - bbox.minXM);
    const spanY = Math.max(1, bbox.maxYM - bbox.minYM);
    const radius = Math.hypot(spanX, spanY) / 2;

    this.ground.position.set(cx, cy, bbox.minZM - 0.05);
    this.ground.updateMatrix();
    this.#resizeGround(radius);
    this.sky.scale.setScalar(Math.max(4000, radius * 8));
    this.sky.updateMatrix();

    const tile = this.#options.tileSizeM;
    const tilesX = Math.max(1, Math.ceil(spanX / tile));
    const tilesY = Math.max(1, Math.ceil(spanY / tile));
    const tileIndex = (x: number, y: number): number => {
      const ix = Math.min(tilesX - 1, Math.max(0, Math.floor((x - bbox.minXM) / tile)));
      const iy = Math.min(tilesY - 1, Math.max(0, Math.floor((y - bbox.minYM) / tile)));
      return iy * tilesX + ix;
    };

    const nTiles = tilesX * tilesY;
    const surfaces: (MeshBuilder | null)[] = new Array<MeshBuilder | null>(nTiles).fill(null);
    const marks: (MeshBuilder | null)[] = new Array<MeshBuilder | null>(nTiles).fill(null);
    const surfaceOf = (i: number): MeshBuilder => {
      let b = surfaces[i];
      if (!b) {
        b = new MeshBuilder({ color: true, vertexCapacity: 4096, indexCapacity: 8192 });
        surfaces[i] = b;
      }
      return b;
    };
    const markOf = (i: number): MeshBuilder => {
      let b = marks[i];
      if (!b) {
        b = new MeshBuilder({ color: true, vertexCapacity: 2048, indexCapacity: 4096 });
        marks[i] = b;
      }
      return b;
    };

    const th = this.#theme;
    const scratch: number[] = [];

    // ---- Landuse first (lowest layer), then lanes, junctions, crossings. ----
    const ring = world.ringPoints;
    for (let i = 0; i < world.landuse.count; i++) {
      const lu = world.landuse.at(i);
      if (lu.ringCount < 3) continue;
      const hex = landuseColor(th, lu.classIdx);
      const [r, g, b] = colorTriple(hex);
      const t = tileIndex(ring.x[lu.ringOff], ring.y[lu.ringOff]);
      addPolygon(surfaceOf(t), ring.x, ring.y, lu.ringOff, lu.ringCount, bbox.minZM + Z_LANDUSE, scratch, r, g, b);
    }

    const lanes = world.lanes;
    const pts = world.lanePoints;
    let laneCount = 0;
    for (let i = 0; i < lanes.count; i++) {
      const n = lanes.pointCount[i];
      if (n < 2) continue;
      const off = lanes.pointOff[i];
      const type = lanes.laneType[i];
      const halfWidth = Math.max(0.4, lanes.widthM[i] / 2);
      const t = tileIndex(pts.x[off], pts.y[off]);
      const surface = surfaceOf(t);
      let hex = th.road;
      let z = Z_ROAD;
      switch (type) {
        case LANE_SIDEWALK: hex = th.sidewalk; z = Z_ROAD + 0.04; break;
        case LANE_BIKE: hex = th.bikeLane; break;
        case LANE_BUS: hex = th.busLane; break;
        case LANE_PARKING: hex = th.parking; break;
        case LANE_JUNCTION_INTERNAL: hex = th.junction; z = Z_JUNCTION; break;
        case LANE_CROSSING: hex = th.crossing; z = Z_CROSSING; break;
        default: break;
      }
      const [r, g, b] = colorTriple(hex);
      addRibbon(surface, pts.x, pts.y, pts.z, off, n, halfWidth, z, r, g, b);
      laneCount++;

      if (this.#options.laneMarkings && (type === LANE_DRIVE || type === LANE_BUS)) {
        const mb = markOf(t);
        const edge = lanes.indexInEdge[i];
        const [mr, mg, mb2] = colorTriple(edge === 0 ? th.laneMarking : th.laneMarking);
        // Outer edge line on the rightmost lane, dashed separator elsewhere.
        addOffsetLine(mb, pts.x, pts.y, pts.z, off, n, halfWidth - 0.12, 0.09, Z_MARKING, mr, mg, mb2);
        if (edge === 0) {
          const [cr, cg, cb] = colorTriple(th.laneMarkingCentre);
          addOffsetLine(mb, pts.x, pts.y, pts.z, off, n, -(halfWidth - 0.12), 0.09, Z_MARKING, cr, cg, cb);
        }
      }
    }

    for (let i = 0; i < world.junctions.count; i++) {
      const j = world.junctions.at(i);
      const r = Math.max(4, Math.min(30, 2.2 + j.laneCount * 1.1));
      const [cr, cg, cb] = colorTriple(th.junction);
      addDisc(surfaceOf(tileIndex(j.xM, j.yM)), j.xM, j.yM, bbox.minZM + Z_JUNCTION, r, 16, cr, cg, cb);
    }

    for (let i = 0; i < world.crossings.count; i++) {
      const c = world.crossings.at(i);
      const dx = c.x2M - c.x1M;
      const dy = c.y2M - c.y1M;
      const len = Math.hypot(dx, dy);
      if (len < 0.2) continue;
      const [cr, cg, cb] = colorTriple(th.crossing);
      // Zebra: bars across the crossing axis.
      const bars = Math.max(2, Math.min(14, Math.round(len / 0.9)));
      const ux = dx / len;
      const uy = dy / len;
      const px = -uy;
      const py = ux;
      const hw = Math.max(0.6, c.widthM / 2);
      const mb = markOf(tileIndex(c.x1M, c.y1M));
      for (let k = 0; k < bars; k++) {
        const f = (k + 0.5) / bars;
        const bx = c.x1M + dx * f;
        const by = c.y1M + dy * f;
        addQuadStrip(mb, bx, by, ux, uy, px, py, len / bars * 0.45, hw, bbox.minZM + Z_CROSSING, cr, cg, cb);
      }
    }

    // ---- Buildings. ----
    if (this.#options.buildings && world.buildings.count > 0) {
      this.#buildBuildings(world, surfaceOf, tileIndex);
    }

    // ---- Publish the tile meshes. ----
    let surfaceVerts = 0;
    let markVerts = 0;
    let drawables = 0;
    for (let i = 0; i < nTiles; i++) {
      const sb = surfaces[i];
      if (sb && !sb.empty) {
        surfaceVerts += sb.vertexCount;
        const g = sb.toGeometry();
        if (g) {
          const m = new Mesh(g, this.#surfaceMaterial);
          m.name = `world/tile-${i}`;
          m.receiveShadow = this.#options.shadows;
          m.matrixAutoUpdate = false;
          this.tiles.add(m);
          this.#disposables.push(g);
          drawables++;
        }
      }
      const mbld = marks[i];
      if (mbld && !mbld.empty) {
        markVerts += mbld.vertexCount;
        const g = mbld.toGeometry();
        if (g) {
          const m = new Mesh(g, this.#markingMaterial);
          m.name = `world/markings-${i}`;
          m.matrixAutoUpdate = false;
          this.markings.add(m);
          this.#disposables.push(g);
          drawables++;
        }
      }
    }

    this.signals.build(world);
    this.#buildSites(world);
    if (this.#buildings) drawables++;
    if (this.signals.count > 0) drawables += 3;
    drawables += this.sitesGroup.children.length + 2; // ground and sky

    this.#report = {
      tiles: nTiles,
      lanes: laneCount,
      buildings: this.#buildingCount,
      junctions: world.junctions.count,
      signals: world.signals.count,
      sites: world.sites.count,
      crossings: world.crossings.count,
      landuse: world.landuse.count,
      buildingBackend: this.#buildingBackend,
      buildingVertices: this.#report.buildingVertices,
      buildingIndices: this.#report.buildingIndices,
      surfaceVertices: surfaceVerts,
      markingVertices: markVerts,
      drawables,
      buildMs: nowMs() - t0,
    };
  }

  #buildBuildings(
    world: VwpWorld,
    surfaceOf: (tile: number) => MeshBuilder,
    tileIndex: (x: number, y: number) => number,
  ): void {
    const b = world.buildings;
    const ring = world.ringPoints;
    const B = b.count;
    const [wr, wg, wb] = colorTriple(this.#theme.building);
    const [rr, rg, rb] = colorTriple(this.#theme.buildingRoof);
    const scratch: number[] = [];
    // One mutable shading record, rewritten per building: 3,024 buildings × 3 LODs would otherwise
    // be 9,072 short-lived objects during a world build.
    const shading: { tone: number; baseOcclusion: number; faceRelief: number; parapetGain: number } = {
      tone: 1,
      baseOcclusion: this.#options.buildingBaseOcclusion,
      faceRelief: this.#options.buildingFaceRelief,
      parapetGain: 1.16,
    };
    const toneAmp = this.#options.buildingToneVariation;
    /** Deterministic per-building tone, so a rebuild or a theme swap never reshuffles the city. */
    const toneOf = (i: number): number => 1 + toneAmp * (hash01(b.buildingId[i], i) * 2 - 1);

    // Pass 1: measure, so the BatchedMesh can be sized exactly.
    const probe = new MeshBuilder({ color: true, vertexCapacity: 512, indexCapacity: 1024 });
    let totalV = 0;
    let totalI = 0;
    const perLodV = new Int32Array(B * 3);
    const perLodI = new Int32Array(B * 3);
    for (let i = 0; i < B; i++) {
      const n = b.ringCount[i];
      if (n < 3) continue;
      shading.tone = toneOf(i);
      probe.reset();
      addExtrudedRing(probe, ring.x, ring.y, b.ringOff[i], n, b.baseZM[i], Math.max(1, b.heightM[i]),
        0, scratch, wr, wg, wb, rr, rg, rb, shading);
      perLodV[i * 3] = probe.vertexCount;
      perLodI[i * 3] = probe.indexCount;
      totalV += probe.vertexCount;
      totalI += probe.indexCount;
    }

    this.#buildingCount = B;
    this.#buildingCentroid = new Float32Array(B * 4);
    this.#buildingLod = new Uint8Array(B).fill(255);
    this.#buildingGeomIds = new Int32Array(B * 3).fill(-1);
    this.#buildingInstanceIds = new Int32Array(B).fill(-1);

    for (let i = 0; i < B; i++) {
      const n = b.ringCount[i];
      const off = b.ringOff[i];
      let sx = 0;
      let sy = 0;
      let maxR = 0;
      for (let k = 0; k < n; k++) {
        sx += ring.x[off + k];
        sy += ring.y[off + k];
      }
      const cx = n > 0 ? sx / n : 0;
      const cy = n > 0 ? sy / n : 0;
      for (let k = 0; k < n; k++) {
        const d = Math.hypot(ring.x[off + k] - cx, ring.y[off + k] - cy);
        if (d > maxR) maxR = d;
      }
      this.#buildingCentroid[i * 4] = cx;
      this.#buildingCentroid[i * 4 + 1] = cy;
      this.#buildingCentroid[i * 4 + 2] = b.baseZM[i] + Math.max(1, b.heightM[i]);
      this.#buildingCentroid[i * 4 + 3] = maxR;
    }
    this.#buildGrid(world);

    const useBatched = totalV > 0 && totalV <= this.#options.maxBuildingVertices;
    if (useBatched) {
      let mesh: BatchedMesh | null = null;
      try {
        mesh = new BatchedMesh(B, Math.ceil(totalV * 1.02) + 64, Math.ceil(totalI * 1.02) + 128,
          this.#buildingMaterial);
        mesh.name = "world/buildings";
        mesh.castShadow = this.#options.shadows;
        mesh.receiveShadow = this.#options.shadows;
        mesh.perObjectFrustumCulled = true;
        mesh.sortObjects = false;
        const builder = new MeshBuilder({ color: true, vertexCapacity: 512, indexCapacity: 1024 });
        const identity = this.#scratchMatrix.identity();
        for (let i = 0; i < B; i++) {
          const n = b.ringCount[i];
          if (n < 3) continue;
          shading.tone = toneOf(i);
          builder.reset();
          addExtrudedRing(builder, ring.x, ring.y, b.ringOff[i], n, b.baseZM[i], Math.max(1, b.heightM[i]),
            0, scratch, wr, wg, wb, rr, rg, rb, shading);
          const g = builder.toGeometry();
          if (!g) continue;
          const id = mesh.addGeometry(g);
          g.dispose();
          // Every LOD slot names the one exact geometry; see the file header.
          this.#buildingGeomIds[i * 3] = id;
          this.#buildingGeomIds[i * 3 + 1] = id;
          this.#buildingGeomIds[i * 3 + 2] = id;
          const g0 = this.#buildingGeomIds[i * 3 + 1];
          if (g0 < 0) continue;
          const inst = mesh.addInstance(g0);
          mesh.setMatrixAt(inst, identity);
          this.#buildingInstanceIds[i] = inst;
          this.#buildingLod[i] = 1;
        }
        this.#buildings = mesh;
        this.buildingsGroup.add(mesh);
        this.#buildingBackend = "batched";
        this.#report = { ...this.#report, buildingVertices: totalV, buildingIndices: totalI };
        return;
      } catch (err) {
        // BatchedMesh refused (budget, or a three build without multi-draw support). Fall through to
        // the merged path; the partially built mesh is dropped so it cannot leak a GPU buffer.
        if (mesh) {
          mesh.removeFromParent();
          mesh.dispose();
        }
        this.#buildings = null;
        this.#buildingGeomIds.fill(-1);
        this.#buildingInstanceIds.fill(-1);
        this.#buildingLod.fill(255);
        this.#buildingBackend = "merged";
        this.#buildError = err instanceof Error ? err.message : String(err);
      }
    }

    // Fallback: bake the mid LOD into the per-tile meshes. One extra draw call per tile, no LOD.
    this.#buildingBackend = "merged";
    for (let i = 0; i < B; i++) {
      const n = b.ringCount[i];
      if (n < 3) continue;
      const off = b.ringOff[i];
      const t = tileIndex(ring.x[off], ring.y[off]);
      shading.tone = toneOf(i);
      addExtrudedRing(surfaceOf(t), ring.x, ring.y, off, n, b.baseZM[i], Math.max(1, b.heightM[i]),
        0, scratch, wr, wg, wb, rr, rg, rb, shading);
    }
    this.#report = { ...this.#report, buildingVertices: totalV, buildingIndices: totalI };
  }

  /** Uniform grid over building footprints, for {@link buildingTopAt}. */
  #buildGrid(world: VwpWorld): void {
    const b = world.buildings;
    const B = b.count;
    const bbox = world.bbox;
    const cell = Math.max(10, Math.min(60, Math.hypot(bbox.maxXM - bbox.minXM, bbox.maxYM - bbox.minYM) / 120));
    this.#gridCell = cell;
    this.#gridMinX = bbox.minXM;
    this.#gridMinY = bbox.minYM;
    this.#gridW = Math.max(1, Math.ceil((bbox.maxXM - bbox.minXM) / cell) + 1);
    this.#gridH = Math.max(1, Math.ceil((bbox.maxYM - bbox.minYM) / cell) + 1);
    const cells = this.#gridW * this.#gridH;
    const counts = new Int32Array(cells + 1);

    const cellRange = (i: number): [number, number, number, number] => {
      const cx = this.#buildingCentroid[i * 4];
      const cy = this.#buildingCentroid[i * 4 + 1];
      const r = this.#buildingCentroid[i * 4 + 3];
      const x0 = Math.max(0, Math.floor((cx - r - this.#gridMinX) / cell));
      const x1 = Math.min(this.#gridW - 1, Math.floor((cx + r - this.#gridMinX) / cell));
      const y0 = Math.max(0, Math.floor((cy - r - this.#gridMinY) / cell));
      const y1 = Math.min(this.#gridH - 1, Math.floor((cy + r - this.#gridMinY) / cell));
      return [x0, x1, y0, y1];
    };

    let total = 0;
    for (let i = 0; i < B; i++) {
      if (b.ringCount[i] < 3) continue;
      const [x0, x1, y0, y1] = cellRange(i);
      for (let y = y0; y <= y1; y++) {
        for (let x = x0; x <= x1; x++) {
          counts[y * this.#gridW + x + 1]++;
          total++;
        }
      }
    }
    for (let i = 0; i < cells; i++) counts[i + 1] += counts[i];
    const items = new Int32Array(total);
    const cursor = counts.slice(0, cells);
    for (let i = 0; i < B; i++) {
      if (b.ringCount[i] < 3) continue;
      const [x0, x1, y0, y1] = cellRange(i);
      for (let y = y0; y <= y1; y++) {
        for (let x = x0; x <= x1; x++) {
          const c = y * this.#gridW + x;
          items[cursor[c]++] = i;
        }
      }
    }
    this.#gridStart = counts;
    this.#gridItems = items;
  }

  /**
   * The top of the building covering `(x, y)`, or `-Infinity` when the point is outside every
   * footprint. `cameras.ts` calls this to keep the chase camera out of geometry.
   */
  buildingTopAt(x: number, y: number): number {
    const i = this.#buildingAt(x, y, true);
    return i < 0 ? -Infinity : this.#buildingCentroid[i * 4 + 2];
  }

  /**
   * Index of the tallest building whose footprint covers `(x, y)`, or −1. Unlike
   * {@link buildingTopAt} this also reports a ghosted building.
   */
  buildingIndexAt(x: number, y: number): number {
    return this.#buildingAt(x, y, false);
  }

  /** The `building_id` of building `i`, or −1. */
  buildingIdOf(i: number): number {
    const w = this.#world;
    return w && i >= 0 && i < w.buildings.count ? w.buildings.buildingId[i] : -1;
  }

  /** Roof height of building `i` (base + height), or −∞. */
  buildingTopOf(i: number): number {
    return i >= 0 && i < this.#buildingCount ? this.#buildingCentroid[i * 4 + 2] : -Infinity;
  }

  /** The building currently ghosted, or −1. */
  get ghostBuilding(): number {
    return this.#ghost;
  }

  /**
   * Hide one building — the one the followed vehicle is driving *through*.
   *
   * The engine's lanes pass through building footprints where the map has a road under a building
   * (a passage, an arcade, a ramp into a terminal: measured on Manhattan, 39 of 2,406 drive lanes
   * cross a footprint for 1.7 km in all). A chase camera behind a car in such a passage used to be
   * thrown onto the roof — 172 m up for one of them — looking down at a roof that hid the car. The
   * building is hidden instead, and the camera logic treats it as open space, for as long as the
   * followed vehicle is inside it. Only the batched backend can hide one building; with the merged
   * fallback this is a no-op and the old roof rule applies.
   */
  setGhostBuilding(index: number): void {
    const i = index >= 0 && index < this.#buildingCount ? index : -1;
    if (i === this.#ghost) return;
    const mesh = this.#buildings;
    if (!mesh) {
      this.#ghost = -1;
      return;
    }
    const prev = this.#ghost;
    if (prev >= 0 && this.#buildingInstanceIds[prev] >= 0) mesh.setVisibleAt(this.#buildingInstanceIds[prev], true);
    if (i >= 0 && this.#buildingInstanceIds[i] >= 0) mesh.setVisibleAt(this.#buildingInstanceIds[i], false);
    this.#ghost = i >= 0 && this.#buildingInstanceIds[i] >= 0 ? i : -1;
  }

  #buildingAt(x: number, y: number, skipGhost: boolean): number {
    const world = this.#world;
    if (!world || this.#gridItems.length === 0) return -1;
    const gx = Math.floor((x - this.#gridMinX) / this.#gridCell);
    const gy = Math.floor((y - this.#gridMinY) / this.#gridCell);
    if (!(gx >= 0 && gy >= 0 && gx < this.#gridW && gy < this.#gridH)) return -1;
    const c = gy * this.#gridW + gx;
    const start = this.#gridStart[c];
    const end = this.#gridStart[c + 1];
    const b = world.buildings;
    const ring = world.ringPoints;
    let top = -Infinity;
    let best = -1;
    for (let k = start; k < end; k++) {
      const i = this.#gridItems[k];
      if (skipGhost && i === this.#ghost) continue;
      const dx = x - this.#buildingCentroid[i * 4];
      const dy = y - this.#buildingCentroid[i * 4 + 1];
      const r = this.#buildingCentroid[i * 4 + 3];
      if (dx * dx + dy * dy > r * r) continue;
      if (pointInRing(ring.x, ring.y, b.ringOff[i], b.ringCount[i], x, y)) {
        const t = this.#buildingCentroid[i * 4 + 2];
        if (t > top) {
          top = t;
          best = i;
        }
      }
    }
    return best;
  }

  #buildSites(world: VwpWorld): void {
    const n = world.sites.count;
    this.#siteCount = n;
    this.#sitePos = new Float32Array(n * 3);
    this.#siteIds = new Uint32Array(n);
    this.#siteNodeIds = new Uint32Array(n);
    this.#siteKinds = new Uint8Array(n);
    if (n === 0) return;
    const builder = new MeshBuilder({ color: true, vertexCapacity: 256, indexCapacity: 512 });
    const [r, g, b] = colorTriple(this.#theme.rsu);
    for (let i = 0; i < n; i++) {
      const s = world.sites.at(i);
      const top = s.zM + Math.max(1, s.antennaHeightM);
      this.#sitePos[i * 3] = s.xM;
      this.#sitePos[i * 3 + 1] = s.yM;
      this.#sitePos[i * 3 + 2] = top;
      this.#siteIds[i] = s.siteId;
      this.#siteNodeIds[i] = s.nodeId;
      this.#siteKinds[i] = s.kind;
      addCylinder(builder, s.xM, s.yM, s.zM, top, 0.18, 6, 0.35, 0.38, 0.42);
      addBox(builder, s.xM, s.yM, top + 0.35, 0.7, 0.28, 0.7, 0, r, g, b);
    }
    const geom = builder.toGeometry();
    if (!geom) return;
    this.#disposables.push(geom);
    const mesh = new Mesh(geom, this.#siteMaterial);
    mesh.name = "world/site-masts";
    mesh.castShadow = false;
    mesh.matrixAutoUpdate = false;
    this.sitesGroup.add(mesh);
  }

  /**
   * Apply signal rows on top of the current state, keyed by controller (`signal_id`), to every head
   * of that controller. Pass `null` to forget every state. Kept for callers that hold a lone block
   * (the replay path); a stream should use {@link applySignalKeyframe} and {@link applySignalDelta},
   * which is what makes a seek or a reconnect come out right.
   */
  updateSignalPhases(block: SignalBlock | null): void {
    if (block) this.signals.applyDelta(block);
    else this.signals.resetStates();
  }

  /** A keyframe's signal block: the complete state (§3.3.3); unmentioned heads have no data. */
  applySignalKeyframe(block: SignalBlock | null): void {
    this.signals.applyKeyframe(block);
  }

  /** A delta's signal block: only the rows that changed (§3.4.7). */
  applySignalDelta(block: SignalBlock | null): void {
    this.signals.applyDelta(block);
  }

  /** Convenience alias matching the message name. */
  updateSignals(block: SignalBlock): void {
    this.updateSignalPhases(block);
  }

  /**
   * Re-pick building LODs for the current camera. Cheap and idempotent: it returns immediately
   * unless the camera has moved at least `moveThresholdM` since the last call, and it only touches
   * the instances whose band actually changed.
   */
  updateLod(camera: Camera, moveThresholdM = 12): number {
    const mesh = this.#buildings;
    if (!mesh) return 0;
    const e = camera.matrixWorld.elements;
    const cx = e[12];
    const cy = e[13];
    const cz = e[14];
    const p = this.#lodCamPos;
    if (Number.isFinite(p.x)) {
      const dx = cx - p.x;
      const dy = cy - p.y;
      const dz = cz - p.z;
      if (dx * dx + dy * dy + dz * dz < moveThresholdM * moveThresholdM) return this.#buildingsVisible;
    }
    p.set(cx, cy, cz);

    const [d0, d1] = this.#options.buildingLodDistancesM;
    const d0Sq = d0 * d0;
    const d1Sq = d1 * d1;
    let visible = 0;
    for (let i = 0; i < this.#buildingCount; i++) {
      const inst = this.#buildingInstanceIds[i];
      if (inst < 0) continue;
      visible++;
      const dx = this.#buildingCentroid[i * 4] - cx;
      const dy = this.#buildingCentroid[i * 4 + 1] - cy;
      const dz = this.#buildingCentroid[i * 4 + 2] - cz;
      const dist = dx * dx + dy * dy + dz * dz;
      const lod = dist < d0Sq ? 0 : dist < d1Sq ? 1 : 2;
      if (this.#buildingLod[i] === lod) continue;
      const gid = this.#buildingGeomIds[i * 3 + lod];
      if (gid < 0) continue;
      mesh.setGeometryIdAt(inst, gid);
      this.#buildingLod[i] = lod;
    }
    this.#buildingsVisible = visible;
    return visible;
  }

  /** Keep the sky centred on the camera so its radius never has to cover the whole world. */
  followCamera(camera: Camera): void {
    const e = camera.matrixWorld.elements;
    this.sky.position.set(e[12], e[13], e[14]);
    this.sky.updateMatrix();
  }

  #clearWorld(): void {
    // The signal renderer rebuilds itself in `setWorld`, keeping what the lamps showed when the
    // world is the same one (a theme swap).
    for (const g of [this.tiles, this.markings, this.buildingsGroup, this.sitesGroup]) {
      for (let i = g.children.length - 1; i >= 0; i--) {
        const child = g.children[i];
        g.remove(child);
        if (child instanceof InstancedMesh || child instanceof BatchedMesh) child.dispose();
      }
    }
    for (const d of this.#disposables) d.dispose();
    this.#disposables = [];
    this.#buildings = null;
    this.#ghost = -1;
    this.#buildingCount = 0;
    this.#buildingBackend = "none";
    this.#buildError = null;
    this.#buildingsVisible = 0;
    this.#lodCamPos.set(NaN, NaN, NaN);
    this.#world = null;
  }

  /** Release every GPU resource. */
  dispose(): void {
    this.#clearWorld();
    // The shadow map and its depth render target belong to the light, not to the renderer:
    // `WebGLRenderer.dispose()` does not touch them, and `LightShadow.dispose()` is the only thing
    // that frees `map` and `mapPass` (three 0.186.0, LightShadow.js). Without this every disposed
    // viewer leaks a 2048² depth target, ~16 MiB (finding Q8).
    this.sun.shadow.dispose();
    this.sun.shadow.map = null;
    this.sun.shadow.mapPass = null;
    this.ground.geometry.dispose();
    this.ground.material.dispose();
    this.sky.geometry.dispose();
    this.sky.material.dispose();
    this.#surfaceMaterial.dispose();
    this.#markingMaterial.dispose();
    this.#buildingMaterial.dispose();
    this.signals.dispose();
    this.#siteMaterial.dispose();
    this.group.removeFromParent();
  }
}

function nowMs(): number {
  return typeof performance !== "undefined" ? performance.now() : Date.now();
}

const TRIPLE_COLOR = new Color();
const TRIPLE_OUT: [number, number, number] = [0, 0, 0];

/**
 * A stable `[0, 1)` from two integers — the per-building tone variation's only source of randomness.
 *
 * Deterministic on purpose: the tone has to survive a theme swap, a reconnect and a second
 * `setWorld` of the same payload, or the city reshuffles itself under the user while they watch.
 */
function hash01(a: number, b: number): number {
  let h = (a | 0) * 0x27d4eb2d ^ (b | 0) * 0x165667b1;
  h = Math.imul(h ^ (h >>> 15), 0x2c1b3c6d);
  h = Math.imul(h ^ (h >>> 12), 0x297a2d39);
  h ^= h >>> 15;
  return (h >>> 8) / 0x1000000;
}

/** Convert a packed sRGB hex to a linear working-space triple. Returns a shared array — copy it. */
function colorTriple(hex: number): [number, number, number] {
  TRIPLE_COLOR.setHex(hex);
  TRIPLE_OUT[0] = TRIPLE_COLOR.r;
  TRIPLE_OUT[1] = TRIPLE_COLOR.g;
  TRIPLE_OUT[2] = TRIPLE_COLOR.b;
  return TRIPLE_OUT;
}

/** §4.5 landuse class → theme colour. */
function landuseColor(theme: ViewerTheme, cls: number): number {
  switch (cls) {
    case 4: return theme.water;
    case 5: return theme.park;
    case 6: return theme.industrial;
    default: return theme.ground;
  }
}

/** A thin line parallel to a polyline, offset `offset` metres to the left. */
function addOffsetLine(
  b: MeshBuilder,
  xs: Float32Array, ys: Float32Array, zs: Float32Array | null,
  off: number, count: number,
  offset: number, halfWidth: number, zOffset: number,
  r: number, g: number, bl: number,
): void {
  if (count < 2) return;
  b.reserve(count * 2, (count - 1) * 6);
  let prevL = -1;
  let prevR = -1;
  for (let i = 0; i < count; i++) {
    const k = off + i;
    const x = xs[k];
    const y = ys[k];
    const z = (zs ? zs[k] : 0) + zOffset;
    let dx: number;
    let dy: number;
    if (i === 0) {
      dx = xs[k + 1] - x;
      dy = ys[k + 1] - y;
    } else if (i === count - 1) {
      dx = x - xs[k - 1];
      dy = y - ys[k - 1];
    } else {
      dx = xs[k + 1] - xs[k - 1];
      dy = ys[k + 1] - ys[k - 1];
    }
    const len = Math.hypot(dx, dy) || 1;
    const nx = -dy / len;
    const ny = dx / len;
    const ox = nx * offset;
    const oy = ny * offset;
    const li = b.addVertex(x + ox + nx * halfWidth, y + oy + ny * halfWidth, z, 0, 0, 1, 0, 0, r, g, bl);
    const ri = b.addVertex(x + ox - nx * halfWidth, y + oy - ny * halfWidth, z, 0, 0, 1, 1, 0, r, g, bl);
    if (prevL >= 0) b.addQuad(prevL, prevR, ri, li);
    prevL = li;
    prevR = ri;
  }
}

/** One zebra bar: a quad centred at `(cx, cy)`, `halfLen` along `(ux, uy)`, `halfW` along `(px, py)`. */
function addQuadStrip(
  b: MeshBuilder,
  cx: number, cy: number,
  ux: number, uy: number, px: number, py: number,
  halfLen: number, halfW: number, z: number,
  r: number, g: number, bl: number,
): void {
  const v0 = b.addVertex(cx - ux * halfLen - px * halfW, cy - uy * halfLen - py * halfW, z, 0, 0, 1, 0, 0, r, g, bl);
  const v1 = b.addVertex(cx + ux * halfLen - px * halfW, cy + uy * halfLen - py * halfW, z, 0, 0, 1, 1, 0, r, g, bl);
  const v2 = b.addVertex(cx + ux * halfLen + px * halfW, cy + uy * halfLen + py * halfW, z, 0, 0, 1, 1, 1, r, g, bl);
  const v3 = b.addVertex(cx - ux * halfLen + px * halfW, cy - uy * halfLen + py * halfW, z, 0, 0, 1, 0, 1, r, g, bl);
  b.addQuad(v0, v1, v2, v3);
}

/** Even-odd point-in-polygon over a ring stored in parallel arrays. */
export function pointInRing(
  xs: Float32Array, ys: Float32Array, off: number, count: number, px: number, py: number,
): boolean {
  let inside = false;
  for (let i = 0, j = count - 1; i < count; j = i++) {
    const xi = xs[off + i];
    const yi = ys[off + i];
    const xj = xs[off + j];
    const yj = ys[off + j];
    if ((yi > py) !== (yj > py) && px < ((xj - xi) * (py - yi)) / (yj - yi || 1e-12) + xi) inside = !inside;
  }
  return inside;
}
