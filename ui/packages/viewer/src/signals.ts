/**
 * Traffic-signal heads: what each lantern shows, and the stop bar on the road in front of it.
 *
 * ## Which rows light which heads
 *
 * The world carries one record per *physical head* (§4.5), and several heads share a `signal_id`
 * — the controller's id (`crates/v2xw-world/src/serde_vwp.rs`: "a controller with heads on four
 * approaches produces four records that share its `signal_id`"). A stream signal row (§3.3.3) is
 * keyed by that same id, so it applies to **every** head of the controller. The previous renderer
 * kept a `Map<signal_id, row index>` and let each head overwrite the last: one head per controller
 * ever changed colour and the rest stayed dark, which is "sometimes I see the light turn green and
 * sometimes not" exactly.
 *
 * A keyframe (§3.3.3) is the complete signal state, so it is applied as one: every head not in it
 * goes back to "no data". A delta (§3.4.7) carries only the rows that changed and is applied on top.
 * The {@link SignalRenderer.applyKeyframe}/{@link SignalRenderer.applyDelta} split is what makes
 * the lamps come out right after a seek, a rewind or a reconnect — a lamp that depended on having
 * seen every delta since the last keyframe is the other half of the owner's report.
 *
 * What the stream cannot say, the renderer does not invent: §3.3.3 has one phase per controller,
 * not one per signal group, so every head of a controller shows that controller's phase. If the
 * engine ever publishes per-group state it needs a protocol change (a row per group), not a
 * renderer guess.
 *
 * ## What a lamp looks like
 *
 * A three-aspect vertical lantern, red over amber over green (MUTCD 2009 §4D.11 and Vienna
 * Convention 1968 Art. 23: the arrangement is fixed so position alone carries the meaning), facing
 * the approach it controls. The lit aspect is drawn unlit-shaded at full value, the others at a
 * tenth — so position, colour and brightness all say the same thing and none of them depends on the
 * sun. Flashing states (J2735 `stop-Then-Proceed`, `caution-Conflicting-Traffic`) flash at 1 Hz,
 * inside MUTCD §4D.30's 50–60 flashes a minute. The J2735 `MovementPhaseState` meanings are from SAE
 * J2735 (2016) DE_MovementPhaseState.
 *
 * A stop bar across the controlled lane repeats the state on the road surface, which is the only
 * place it can be read from map altitude where a 1 m lantern is a fraction of a pixel.
 */

import {
  BufferGeometry,
  Color,
  Group,
  InstancedMesh,
  Matrix4,
  MeshBasicMaterial,
  MeshLambertMaterial,
  type Material,
} from "three";
import type { SignalBlock, VwpWorld } from "@vwp/protocol";
import { MeshBuilder, addBox, withUnitVertexColors } from "./geometry.js";
import type { ViewerTheme } from "./theme.js";

/** Aspect bits. */
const RED = 1;
const AMBER = 2;
const GREEN = 4;

/** Phase code for "the stream has said nothing about this head". Not a J2735 value. */
export const PHASE_NO_DATA = 0xff;

/** What a J2735 `MovementPhaseState` lights: aspect bits, and whether they flash. */
export interface SignalAspect {
  readonly lamps: number;
  readonly flashing: boolean;
  /** Short word for the HUD and tests. */
  readonly name: "no-data" | "unavailable" | "dark" | "red" | "red-flashing" | "red-amber" | "green" | "amber" | "amber-flashing";
}

/** SAE J2735 (2016) DE_MovementPhaseState → the aspect a lantern shows. */
export function aspectOf(phase: number): SignalAspect {
  switch (phase) {
    case 0: return { lamps: 0, flashing: false, name: "unavailable" };
    case 1: return { lamps: 0, flashing: false, name: "dark" };
    case 2: return { lamps: RED, flashing: true, name: "red-flashing" }; // stop-Then-Proceed
    case 3: return { lamps: RED, flashing: false, name: "red" }; // stop-And-Remain
    case 4: return { lamps: RED | AMBER, flashing: false, name: "red-amber" }; // pre-Movement
    case 5: // permissive-Movement-Allowed
    case 6: return { lamps: GREEN, flashing: false, name: "green" }; // protected-Movement-Allowed
    case 7: // permissive-clearance
    case 8: return { lamps: AMBER, flashing: false, name: "amber" }; // protected-clearance
    case 9: return { lamps: AMBER, flashing: true, name: "amber-flashing" }; // caution-Conflicting-Traffic
    default: return { lamps: 0, flashing: false, name: "no-data" };
  }
}

/** One head's state, as {@link SignalRenderer.headState} reports it. */
export interface SignalHeadState {
  readonly signalId: number;
  readonly phase: number;
  readonly aspect: SignalAspect;
  /** Deciseconds to the next change as last reported, `0xFFFF` unknown. */
  readonly timeToChangeDs: number;
}

/** Unlit aspects are drawn at this fraction of their lit value. */
const UNLIT = 0.1;
/** Flashing period, seconds (MUTCD §4D.30: 50–60 flashes per minute). */
const FLASH_PERIOD_S = 1;

/** Lantern dimensions, metres: a 12-inch three-section head is about 0.35 × 1.05. */
const HOUSING_W = 0.36;
const HOUSING_H = 1.05;
const HOUSING_D = 0.26;
const LAMP_SPACING = 0.32;
const LAMP_SIZE = 0.24;
/** Stop bar depth along the lane, metres (MUTCD §3B.16: 12–24 inches). */
const STOP_BAR_DEPTH = 0.5;
/** Height of the stop bar above the lane centreline, metres: just above lane markings (0.24). */
const STOP_BAR_Z = 0.26;

export class SignalRenderer {
  readonly group = new Group();

  #housing: InstancedMesh<BufferGeometry, Material> | null = null;
  #lamps: InstancedMesh<BufferGeometry, Material> | null = null;
  #bars: InstancedMesh<BufferGeometry, Material> | null = null;
  #housingMaterial = new MeshLambertMaterial({ color: 0x1b1e22, name: "signal-housing" });
  #lampMaterial = new MeshBasicMaterial({ vertexColors: true, toneMapped: false, name: "signal-lamps" });
  #barMaterial = new MeshBasicMaterial({
    vertexColors: true, toneMapped: false, name: "signal-stop-bars",
    polygonOffset: true, polygonOffsetFactor: -4, polygonOffsetUnits: -4,
  });
  #geometries: BufferGeometry[] = [];

  #count = 0;
  #headSignal = new Uint32Array(0);
  #phase = new Uint8Array(0);
  #ttc = new Uint16Array(0);
  /** signal_id → head indices. */
  #heads = new Map<number, number[]>();
  #hasBar = new Uint8Array(0);
  #colors = { red: new Color(), amber: new Color(), green: new Color() };
  #scratch = new Color();
  #flashOn = true;
  #anyFlashing = false;
  #theme: ViewerTheme;
  /** The world last built; a rebuild of the same one keeps the lamp states. */
  #builtFrom: VwpWorld | null = null;

  constructor(theme: ViewerTheme) {
    this.#theme = theme;
    this.group.name = "world/signals";
    this.setTheme(theme);
  }

  /** Number of heads built. */
  get count(): number {
    return this.#count;
  }

  setTheme(theme: ViewerTheme): void {
    this.#theme = theme;
    this.#colors.red.setHex(theme.signalRed);
    this.#colors.amber.setHex(theme.signalAmber);
    this.#colors.green.setHex(theme.signalGreen);
    this.#repaint();
  }

  /**
   * Build one lantern and one stop bar per §4.5 signal record. What each controller showed is kept
   * across a rebuild — a theme swap rebuilds the world, and must not blank every lamp until the
   * next keyframe (which, on a paused run, never comes).
   */
  build(world: VwpWorld): void {
    const kept = new Map<number, { phase: number; ttc: number }>();
    for (let i = 0; world === this.#builtFrom && i < this.#count; i++) {
      if (this.#phase[i] !== PHASE_NO_DATA) kept.set(this.#headSignal[i], { phase: this.#phase[i], ttc: this.#ttc[i] });
    }
    this.clear();
    this.#builtFrom = world;
    const n = world.signals.count;
    this.#count = n;
    this.#headSignal = new Uint32Array(n);
    this.#phase = new Uint8Array(n).fill(PHASE_NO_DATA);
    this.#ttc = new Uint16Array(n).fill(0xffff);
    this.#hasBar = new Uint8Array(n);
    this.#heads.clear();
    if (n === 0) return;

    // The approach lane's end: where the stop line is and which way the head must face.
    const laneIndex = new Map<number, number>();
    for (let i = 0; i < world.lanes.count; i++) laneIndex.set(world.lanes.laneId[i], i);

    const housingGeom = boxGeometry(HOUSING_D, HOUSING_W, HOUSING_H);
    // Lamps are thin slabs on the face towards the approach (local −x).
    const lampGeom = withUnitVertexColors(boxGeometry(0.03, LAMP_SIZE, LAMP_SIZE));
    const barGeom = withUnitVertexColors(boxGeometry(1, 1, 0.02));
    this.#geometries.push(housingGeom, lampGeom, barGeom);

    const housing = new InstancedMesh<BufferGeometry, Material>(housingGeom, this.#housingMaterial, n);
    const lamps = new InstancedMesh<BufferGeometry, Material>(lampGeom, this.#lampMaterial, n * 3);
    const bars = new InstancedMesh<BufferGeometry, Material>(barGeom, this.#barMaterial, n);
    housing.name = "world/signal-housings";
    lamps.name = "world/signal-lamps";
    bars.name = "world/signal-stop-bars";
    for (const m of [housing, lamps, bars]) {
      m.castShadow = false;
      m.receiveShadow = false;
    }

    const m = new Matrix4();
    const lx = world.lanePoints.x;
    const ly = world.lanePoints.y;
    const lz = world.lanePoints.z;
    for (let i = 0; i < n; i++) {
      const s = world.signals.at(i);
      this.#headSignal[i] = s.signalId;
      let list = this.#heads.get(s.signalId);
      if (!list) {
        list = [];
        this.#heads.set(s.signalId, list);
      }
      list.push(i);

      let yaw = 0;
      let bar: { x: number; y: number; z: number; width: number } | null = null;
      const li = laneIndex.get(s.laneId);
      if (li !== undefined && world.lanes.pointCount[li] >= 2) {
        const off = world.lanes.pointOff[li];
        const last = off + world.lanes.pointCount[li] - 1;
        const dx = lx[last] - lx[last - 1];
        const dy = ly[last] - ly[last - 1];
        const len = Math.hypot(dx, dy);
        if (len > 1e-6) {
          yaw = Math.atan2(dy, dx);
          const ux = dx / len;
          const uy = dy / len;
          bar = {
            x: lx[last] - ux * STOP_BAR_DEPTH * 0.5,
            y: ly[last] - uy * STOP_BAR_DEPTH * 0.5,
            z: (lz ? lz[last] : 0) + STOP_BAR_Z,
            width: Math.max(1, world.lanes.widthM[li] - 0.2),
          };
        }
      }
      const c = Math.cos(yaw);
      const sn = Math.sin(yaw);
      // Housing: local +x along the approach's travel direction, so its −x face looks back at the
      // drivers who have to read it.
      m.set(
        c, -sn, 0, s.xM,
        sn, c, 0, s.yM,
        0, 0, 1, s.zM,
        0, 0, 0, 1,
      );
      housing.setMatrixAt(i, m);
      for (let k = 0; k < 3; k++) {
        const dz = (1 - k) * LAMP_SPACING;
        const fx = -(HOUSING_D / 2 + 0.02);
        m.set(
          c, -sn, 0, s.xM + c * fx,
          sn, c, 0, s.yM + sn * fx,
          0, 0, 1, s.zM + dz,
          0, 0, 0, 1,
        );
        lamps.setMatrixAt(i * 3 + k, m);
      }
      if (bar) {
        // Unit box scaled to the bar: depth along the lane, width across it.
        m.set(
          c * STOP_BAR_DEPTH, -sn * bar.width, 0, bar.x,
          sn * STOP_BAR_DEPTH, c * bar.width, 0, bar.y,
          0, 0, 1, bar.z,
          0, 0, 0, 1,
        );
        this.#hasBar[i] = 1;
      } else {
        m.makeScale(0, 0, 0);
      }
      bars.setMatrixAt(i, m);
    }
    for (const mesh of [housing, lamps, bars]) {
      mesh.instanceMatrix.needsUpdate = true;
      mesh.computeBoundingSphere();
    }
    this.#housing = housing;
    this.#lamps = lamps;
    this.#bars = bars;
    this.group.add(housing, lamps, bars);
    for (let i = 0; i < n; i++) {
      const k = kept.get(this.#headSignal[i]);
      if (k) {
        this.#phase[i] = k.phase;
        this.#ttc[i] = k.ttc;
      }
    }
    this.#repaint();
  }

  /**
   * A keyframe's signal block: the complete state (§3.3.3). Every head the block does not mention
   * goes back to "no data", so nothing survives from before a seek.
   */
  applyKeyframe(block: SignalBlock | null): void {
    this.#phase.fill(PHASE_NO_DATA);
    this.#ttc.fill(0xffff);
    if (block) this.#applyRows(block);
    this.#repaint();
  }

  /** A delta's signal block: only the rows that changed (§3.4.7). */
  applyDelta(block: SignalBlock | null): void {
    if (!block || block.count === 0) return;
    this.#applyRows(block);
    this.#repaint();
  }

  /** Forget every state (a new run). */
  resetStates(): void {
    this.#phase.fill(PHASE_NO_DATA);
    this.#ttc.fill(0xffff);
    this.#repaint();
  }

  #applyRows(block: SignalBlock): void {
    for (let r = 0; r < block.count; r++) {
      const heads = this.#heads.get(block.signalId[r]);
      if (!heads) continue;
      const phase = block.phase[r];
      const ttc = block.timeToChangeDs[r];
      for (const h of heads) {
        this.#phase[h] = phase;
        this.#ttc[h] = ttc;
      }
    }
  }

  /** What head `i` shows. */
  headState(i: number): SignalHeadState | null {
    if (i < 0 || i >= this.#count) return null;
    return {
      signalId: this.#headSignal[i],
      phase: this.#phase[i],
      aspect: aspectOf(this.#phase[i]),
      timeToChangeDs: this.#ttc[i],
    };
  }

  /** The colour actually written for head `i`'s lamp `k` (0 red, 1 amber, 2 green), for tests. */
  lampColor(i: number, k: number): [number, number, number] | null {
    const lamps = this.#lamps;
    if (!lamps?.instanceColor || i < 0 || i >= this.#count) return null;
    const a = lamps.instanceColor.array as Float32Array;
    const o = (i * 3 + k) * 3;
    return [a[o], a[o + 1], a[o + 2]];
  }

  /** Advance flashing aspects. Cheap: returns at once unless a head is flashing. */
  update(timeSeconds: number): void {
    if (!this.#anyFlashing) return;
    const on = ((timeSeconds / FLASH_PERIOD_S) % 1 + 1) % 1 < 0.5;
    if (on === this.#flashOn) return;
    this.#flashOn = on;
    this.#repaint();
  }

  #repaint(): void {
    const lamps = this.#lamps;
    const bars = this.#bars;
    if (!lamps || !bars) return;
    const c = this.#scratch;
    const off = this.#theme.signalDark;
    let flashing = false;
    for (let i = 0; i < this.#count; i++) {
      const aspect = aspectOf(this.#phase[i]);
      if (aspect.flashing) flashing = true;
      const lit = aspect.flashing && !this.#flashOn ? 0 : aspect.lamps;
      const lampColors = [this.#colors.red, this.#colors.amber, this.#colors.green];
      for (let k = 0; k < 3; k++) {
        const bit = k === 0 ? RED : k === 1 ? AMBER : GREEN;
        c.copy(lampColors[k]);
        if (!(lit & bit)) c.multiplyScalar(UNLIT);
        lamps.setColorAt(i * 3 + k, c);
      }
      // The bar says the same thing on the road. "No data" and "dark" draw no bar at all: a grey
      // bar would read as a state.
      if (aspect.lamps === 0 || !this.#hasBar[i]) {
        c.setHex(off);
      } else if (aspect.lamps & RED) {
        c.copy(this.#colors.red);
      } else if (aspect.lamps & AMBER) {
        c.copy(this.#colors.amber);
      } else {
        c.copy(this.#colors.green);
      }
      if (aspect.flashing && !this.#flashOn) c.multiplyScalar(0.35);
      bars.setColorAt(i, c);
    }
    this.#anyFlashing = flashing;
    if (lamps.instanceColor) lamps.instanceColor.needsUpdate = true;
    if (bars.instanceColor) bars.instanceColor.needsUpdate = true;
    // Bars with no state are hidden rather than painted grey.
    let anyBar = false;
    for (let i = 0; i < this.#count; i++) if (this.#hasBar[i] && aspectOf(this.#phase[i]).lamps !== 0) anyBar = true;
    bars.visible = anyBar;
  }

  clear(): void {
    for (const mesh of [this.#housing, this.#lamps, this.#bars]) {
      if (!mesh) continue;
      this.group.remove(mesh);
      mesh.dispose();
    }
    for (const g of this.#geometries) g.dispose();
    this.#geometries = [];
    this.#housing = null;
    this.#lamps = null;
    this.#bars = null;
  }

  dispose(): void {
    this.clear();
    this.#count = 0;
    this.#heads.clear();
    this.#builtFrom = null;
    this.#housingMaterial.dispose();
    this.#lampMaterial.dispose();
    this.#barMaterial.dispose();
  }
}

function boxGeometry(sx: number, sy: number, sz: number): BufferGeometry {
  const b = new MeshBuilder({ vertexCapacity: 32, indexCapacity: 64 });
  addBox(b, 0, 0, 0, sx, sy, sz, 0);
  const g = b.toGeometry();
  if (!g) throw new Error("signal geometry is empty");
  return g;
}
