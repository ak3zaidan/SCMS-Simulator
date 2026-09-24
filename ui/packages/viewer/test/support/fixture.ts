/**
 * Synthetic fixtures built through the real protocol encoders, so the viewer is fed bytes that a
 * conforming server could have sent rather than hand-rolled objects: the world goes through
 * `encodeWorld` → `decodeWorld` (§4), and poses go through `keyframeFrame`/`deltaFrame` →
 * `viewFrame` → `decodeKeyframe`/`decodeDelta` → `PoseBuffer` (§3.3, §3.4).
 */

import { createHash } from "node:crypto";
import {
  ActorState,
  MM_PER_CM,
  PoseBuffer,
  decodeDelta,
  decodeKeyframe,
  decodeWorld,
  deltaFrame,
  encodeWorld,
  keyframeFrame,
  quantiseAccelCq,
  quantiseHeadingBrad,
  quantisePositionMm,
  quantiseHeightCm,
  quantiseSpeedCq,
  viewFrame,
  type DeltaInit,
  type KeyframeInit,
  type VwpWorld,
  type WorldBuildingInit,
  type WorldInit,
  type WorldLaneInit,
} from "@vwp/protocol";

function sha256(bytes: Uint8Array): Uint8Array {
  return new Uint8Array(createHash("sha256").update(bytes).digest());
}

/** Shape of the generated grid world. */
export interface GridWorldOptions {
  /** Streets per axis. Default 12 → 144 blocks. */
  readonly blocks?: number;
  /** Block pitch in metres. Default 120. */
  readonly blockM?: number;
  /** Lanes per direction on each street. Default 2. */
  readonly lanesPerDirection?: number;
  /** Buildings per block. Default 1. */
  readonly buildingsPerBlock?: number;
  /**
   * One `signal_id` per junction shared by its four heads, as the engine writes it (a controller
   * with heads on four approaches, `crates/v2xw-world/src/serde_vwp.rs`), instead of one per head.
   * Default false.
   */
  readonly controllerSignals?: boolean;
}

/** A decoded synthetic world plus the axis positions its streets sit on. */
export interface GridWorld {
  readonly world: VwpWorld;
  readonly bytes: Uint8Array;
  /** ENU coordinate of each street centreline, both axes. */
  readonly axes: number[];
  readonly options: Required<GridWorldOptions>;
}

/**
 * A Manhattan-like grid: `blocks` streets each way, four drive lanes and two sidewalks per street,
 * a building per block, a signalised junction at every intersection, an RSU every third junction, a
 * crossing on each junction approach and one park.
 */
export function makeGridWorld(options: GridWorldOptions = {}): GridWorld {
  const o: Required<GridWorldOptions> = {
    blocks: options.blocks ?? 12,
    blockM: options.blockM ?? 120,
    lanesPerDirection: options.lanesPerDirection ?? 2,
    buildingsPerBlock: options.buildingsPerBlock ?? 1,
    controllerSignals: options.controllerSignals ?? false,
  };
  const n = o.blocks;
  const pitch = o.blockM;
  const span = (n - 1) * pitch;
  const half = span / 2;
  const axes: number[] = [];
  for (let i = 0; i < n; i++) axes.push(-half + i * pitch);

  const lanes: WorldLaneInit[] = [];
  const buildings: WorldBuildingInit[] = [];
  const junctions: WorldInit["junctions"][number][] = [];
  const signals: WorldInit["signals"][number][] = [];
  const sites: WorldInit["sites"][number][] = [];
  const crossings: WorldInit["crossings"][number][] = [];
  const landuse: WorldInit["landuse"][number][] = [];
  const strings = ["", "Grid Street", "Grid Avenue"];

  let laneId = 0;
  const laneWidth = 3.3;
  const addStreet = (horizontal: boolean, axis: number, edgeId: number): void => {
    for (let dir = 0; dir < 2; dir++) {
      for (let k = 0; k < o.lanesPerDirection; k++) {
        const offset = (k + 0.5) * laneWidth * (dir === 0 ? 1 : -1);
        const a = dir === 0 ? -half - pitch / 2 : half + pitch / 2;
        const b = dir === 0 ? half + pitch / 2 : -half - pitch / 2;
        const points: [number, number, number][] = [];
        const steps = n * 2;
        for (let s = 0; s <= steps; s++) {
          const t = a + ((b - a) * s) / steps;
          points.push(horizontal ? [t, axis + offset, 0] : [axis - offset, t, 0]);
        }
        lanes.push({
          laneId: laneId++,
          edgeId,
          junctionId: 0xffffffff,
          strName: horizontal ? 1 : 2,
          widthM: laneWidth,
          speedLimitMps: 13.89,
          allowedClasses: 0b0100_1111,
          laneType: 0,
          indexInEdge: k,
          points,
        });
      }
    }
    // A sidewalk on each side.
    for (const side of [-1, 1]) {
      const offset = side * (o.lanesPerDirection * laneWidth + 1.6);
      const points: [number, number, number][] = [];
      for (let s = 0; s <= 2; s++) {
        const t = -half - pitch / 2 + ((span + pitch) * s) / 2;
        points.push(horizontal ? [t, axis + offset, 0.05] : [axis + offset, t, 0.05]);
      }
      lanes.push({
        laneId: laneId++, edgeId, junctionId: 0xffffffff, strName: horizontal ? 1 : 2,
        widthM: 2.4, speedLimitMps: 1.4, allowedClasses: 0b0010_0000, laneType: 2, indexInEdge: 0, points,
      });
    }
  };

  let edgeId = 0;
  for (let i = 0; i < n; i++) addStreet(true, axes[i], edgeId++);
  for (let i = 0; i < n; i++) addStreet(false, axes[i], edgeId++);

  let junctionId = 0;
  let signalId = 0;
  let siteId = 0;
  let crossingId = 0;
  for (let iy = 0; iy < n; iy++) {
    for (let ix = 0; ix < n; ix++) {
      const x = axes[ix];
      const y = axes[iy];
      junctions.push({ junctionId, strName: 0, xM: x, yM: y, zM: 0, control: 2, laneCount: 8 });
      for (let s = 0; s < 4; s++) {
        const a = (s / 4) * Math.PI * 2;
        signals.push({
          signalId: o.controllerSignals ? junctionId : signalId++, junctionId, laneId: 0,
          xM: x + Math.cos(a) * 9, yM: y + Math.sin(a) * 9, zM: 5.2,
          kind: 0, group: s,
        });
        crossings.push({
          crossingId: crossingId++, junctionId,
          x1M: x + Math.cos(a) * 11 - Math.sin(a) * 6,
          y1M: y + Math.sin(a) * 11 + Math.cos(a) * 6,
          x2M: x + Math.cos(a) * 11 + Math.sin(a) * 6,
          y2M: y + Math.sin(a) * 11 - Math.cos(a) * 6,
          widthM: 3.2,
        });
      }
      if ((ix + iy * n) % 3 === 0) {
        sites.push({
          siteId: siteId++, nodeId: 0x1000 + siteId, xM: x + 12, yM: y + 12, zM: 0,
          antennaHeightM: 6, antennaGainDbi: 5, kind: 0,
        });
      }
      junctionId++;
    }
  }

  let buildingId = 0;
  for (let iy = 0; iy + 1 < n; iy++) {
    for (let ix = 0; ix + 1 < n; ix++) {
      const x0 = axes[ix] + 16;
      const y0 = axes[iy] + 16;
      const x1 = axes[ix + 1] - 16;
      const y1 = axes[iy + 1] - 16;
      const cols = o.buildingsPerBlock;
      for (let b = 0; b < cols; b++) {
        const fx0 = x0 + ((x1 - x0) * b) / cols + 2;
        const fx1 = x0 + ((x1 - x0) * (b + 1)) / cols - 2;
        const levels = 2 + ((ix * 7 + iy * 13 + b * 5) % 18);
        buildings.push({
          buildingId: buildingId++,
          heightM: levels * 3.2,
          baseZM: 0,
          strName: 0,
          material: 1,
          lodHint: levels > 10 ? 1 : 0,
          levels,
          // CCW, unclosed (§4.4).
          ring: [[fx0, y0], [fx1, y0], [fx1, y1], [fx0, y1]],
        });
      }
    }
  }

  landuse.push({
    landuseId: 0,
    // §4.5: 0 urban, 1 suburban, 2 rural, 3 highway, 4 water, 5 park, 6 industrial.
    classIdx: 5,
    ring: [
      [axes[1] + 16, axes[1] + 16], [axes[2] - 16, axes[1] + 16],
      [axes[2] - 16, axes[2] - 16], [axes[1] + 16, axes[2] - 16],
    ],
  });

  const margin = pitch;
  const init: WorldInit = {
    originLatDeg: 40.7527, originLonDeg: -73.979, originAltM: 10,
    bboxMinXM: -half - margin, bboxMinYM: -half - margin,
    bboxMaxXM: half + margin, bboxMaxYM: half + margin,
    bboxMinZM: -1, bboxMaxZM: 70,
    lanes, buildings, junctions, signals, sites, crossings, landuse, strings,
    provenanceJson: JSON.stringify({ source: "synthetic", transformations: ["grid"] }),
  };
  const bytes = encodeWorld(init, sha256);
  const world = decodeWorld(bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer);
  return { world, bytes, axes, options: o };
}

/** A running crowd of actors that produces real `Keyframe` and `Delta` frames. */
export class SyntheticStream {
  readonly count: number;
  readonly poses: PoseBuffer;

  #x: Float64Array;
  #y: Float64Array;
  #z: Float64Array;
  #heading: Float64Array;
  #speed: Float64Array;
  #state: Uint8Array;
  #classIdx: Uint8Array;
  #refX: Int32Array;
  #refY: Int32Array;
  #refZ: Int16Array;
  #originX = 0;
  #originY = 0;
  #originZ = 0;
  #gop = 0;
  #step = 0;
  #simNs = 0n;
  #seq = 1n;
  #half: number;

  constructor(count: number, grid: GridWorld, classCount = 8) {
    this.count = count;
    this.poses = new PoseBuffer(Math.max(1024, count));
    this.#x = new Float64Array(count);
    this.#y = new Float64Array(count);
    this.#z = new Float64Array(count);
    this.#heading = new Float64Array(count);
    this.#speed = new Float64Array(count);
    this.#state = new Uint8Array(count);
    this.#classIdx = new Uint8Array(count);
    this.#refX = new Int32Array(count);
    this.#refY = new Int32Array(count);
    this.#refZ = new Int16Array(count);
    const axes = grid.axes;
    this.#half = ((grid.options.blocks - 1) * grid.options.blockM) / 2 + grid.options.blockM / 2;
    let seed = 0x2f6e2b1;
    const rnd = (): number => {
      seed = (seed * 1664525 + 1013904223) >>> 0;
      return seed / 0x1_0000_0000;
    };
    for (let i = 0; i < count; i++) {
      const horizontal = rnd() < 0.5;
      const axis = axes[Math.floor(rnd() * axes.length)] + (rnd() < 0.5 ? 1.65 : -1.65);
      const along = -this.#half + rnd() * this.#half * 2;
      this.#x[i] = horizontal ? along : axis;
      this.#y[i] = horizontal ? axis : along;
      this.#z[i] = 0;
      this.#heading[i] = horizontal ? (rnd() < 0.5 ? 0 : Math.PI) : (rnd() < 0.5 ? Math.PI / 2 : -Math.PI / 2);
      this.#speed[i] = 6 + rnd() * 10;
      this.#classIdx[i] = Math.floor(rnd() * classCount);
      const r = rnd();
      this.#state[i] = ActorState.EQUIPPED
        | (r < 0.03 ? ActorState.ATTACKER : 0)
        | (r >= 0.03 && r < 0.05 ? ActorState.REPORTED : 0)
        | (r >= 0.05 && r < 0.06 ? ActorState.REVOKED : 0);
    }
  }

  /** Sim time of the last emitted frame. */
  get simTimeNs(): bigint {
    return this.#simNs;
  }

  /** Advance the crowd by `dt` seconds, wrapping at the world edge. */
  advance(dt: number): void {
    for (let i = 0; i < this.count; i++) {
      this.#x[i] += Math.cos(this.#heading[i]) * this.#speed[i] * dt;
      this.#y[i] += Math.sin(this.#heading[i]) * this.#speed[i] * dt;
      if (this.#x[i] > this.#half) this.#x[i] -= this.#half * 2;
      if (this.#x[i] < -this.#half) this.#x[i] += this.#half * 2;
      if (this.#y[i] > this.#half) this.#y[i] -= this.#half * 2;
      if (this.#y[i] < -this.#half) this.#y[i] += this.#half * 2;
    }
    this.#simNs += BigInt(Math.round(dt * 1e9));
  }

  /** Emit a `Keyframe` and apply it to {@link poses}. */
  keyframe(): void {
    this.#originX = 0;
    this.#originY = 0;
    this.#originZ = 0;
    const actors: KeyframeInit["actors"][number][] = [];
    for (let i = 0; i < this.count; i++) {
      const xMm = quantisePositionMm(this.#x[i] - this.#originX);
      const yMm = quantisePositionMm(this.#y[i] - this.#originY);
      const zCm = quantiseHeightCm(this.#z[i] - this.#originZ);
      this.#refX[i] = xMm;
      this.#refY[i] = yMm;
      this.#refZ[i] = zCm;
      actors.push({
        actorId: i + 1, xMm, yMm, zCm,
        laneId: 0xffffffff,
        headingBrad: quantiseHeadingBrad(this.#heading[i]),
        speedCq: quantiseSpeedCq(this.#speed[i]),
        accelCq: quantiseAccelCq(0),
        classIdx: this.#classIdx[i],
        state: this.#state[i],
        verifiedNeighbors: 12,
      });
    }
    const init: KeyframeInit = {
      simTimeNs: this.#simNs,
      originXM: this.#originX, originYM: this.#originY, originZM: this.#originZ,
      gopIndex: this.#gop,
      profile: 0,
      actors,
      signals: [
        { signalId: 0, timeToChangeDs: 55, phase: 6 },
        { signalId: 1, timeToChangeDs: 55, phase: 3 },
        { signalId: 2, timeToChangeDs: 12, phase: 8 },
      ],
    };
    const frame = keyframeFrame(init, this.#seq++);
    const kf = decodeKeyframe(viewFrame(frame));
    this.poses.applyKeyframe(kf);
    this.#step = 0;
  }

  /** Emit a `Delta` for the current state and apply it to {@link poses}. */
  delta(): void {
    this.#step++;
    const moved: NonNullable<DeltaInit["moved"]>[number][] = [];
    for (let i = 0; i < this.count; i++) {
      const xMm = quantisePositionMm(this.#x[i] - this.#originX);
      const yMm = quantisePositionMm(this.#y[i] - this.#originY);
      const zCm = quantiseHeightCm(this.#z[i] - this.#originZ);
      const dx = xMm - this.#refX[i];
      const dy = yMm - this.#refY[i];
      // §3.4.2: `dz_mm` is millimetres even though the reference `z_cm` is centimetres.
      const dz = (zCm - this.#refZ[i]) * MM_PER_CM;
      if (Math.abs(dx) > 32000 || Math.abs(dy) > 32000 || Math.abs(dz) > 32000) continue;
      this.#refX[i] = xMm;
      this.#refY[i] = yMm;
      this.#refZ[i] = zCm;
      moved.push({
        slot: i, dxMm: dx, dyMm: dy, dzMm: dz,
        headingBrad: quantiseHeadingBrad(this.#heading[i]),
        speedCq: quantiseSpeedCq(this.#speed[i]),
        accelCq: quantiseAccelCq(0),
        state: this.#state[i],
        verifiedNeighbors: 12,
        mflags: 0,
      });
    }
    const init: DeltaInit = {
      simTimeNs: this.#simNs, gopIndex: this.#gop, stepIndex: this.#step, moved,
    };
    const frame = deltaFrame(init, this.#seq++);
    const d = decodeDelta(viewFrame(frame));
    this.poses.applyDelta(d);
  }

  /** Start a new GOP; the next {@link keyframe} carries its index. */
  nextGop(): void {
    this.#gop++;
  }
}
