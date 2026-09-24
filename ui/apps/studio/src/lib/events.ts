/**
 * The scenario timeline's vocabulary, for the event editor and anything else that shows events:
 * the kinds the engine accepts, the parameters a `param.change` may set, and the map lookup that
 * turns a click into a street. What each kind does in the engine is
 * `crates/v2xw-engine/src/timeline.rs`.
 */

import type { VwpWorld } from "@vwp/protocol";

/** One timeline item as the document holds it. */
export type EventItem = { t: number; until?: number; type: string } & Record<string, unknown>;

/** The kinds the engine accepts (`TimelineKind`), with what each needs and whether it can end. */
export const EVENT_KINDS: readonly { type: string; label: string; ends: boolean; help: string }[] = [
  { type: "closure", label: "Road closure", ends: true, help: "Close a road or a lane; vehicles re-plan around it. Until reopens it." },
  { type: "demand.multiplier", label: "Traffic surge", ends: true, help: "Multiply the vehicle arrival rate while it lasts." },
  { type: "weather.front", label: "Weather front", ends: false, help: "The weather drivers and radio links see changes from this instant." },
  { type: "param.change", label: "Change a setting", ends: false, help: "Set one setting the running simulation can take on the fly." },
  { type: "outage", label: "Node outage", ends: true, help: "A node stops transmitting and receiving; until brings it back." },
  { type: "attack.wave", label: "Attack wave", ends: true, help: "The named attacker populations act only while the wave lasts." },
];

/** `crates/v2xw-engine/src/timeline.rs` `LIVE_PARAMS`: what a `param.change` may set, and how far it reaches. */
export const LIVE_PARAMS: readonly { path: string; reach: string }[] = [
  { path: "weather.initial", reach: "everything, now" },
  { path: "weather.intensity", reach: "everything, now" },
  { path: "weather.visibility_m", reach: "everything, now" },
  { path: "weather.surface", reach: "everything, now" },
  { path: "actors.vehicles.demand.rate_veh_per_h", reach: "everything, now" },
  { path: "actors.vehicles.equipped_fraction", reach: "vehicles that enter after it" },
  { path: "actors.vru.device_fraction", reach: "people that enter after it" },
  { path: "security.verification_policy", reach: "nodes that enter after it" },
  { path: "security.pseudonym_change.period_s", reach: "nodes that enter after it" },
  { path: "nodes.default_obu", reach: "nodes that enter after it" },
];

/** The nearest vehicle lane's edge to a map point, with its street name when it has one. */
export function nearestEdge(
  world: VwpWorld,
  x: number,
  y: number,
): { edge: number; name: string; distanceM: number } | null {
  const L = world.lanes;
  const P = world.lanePoints;
  let best: { edge: number; name: string; distanceM: number } | null = null;
  for (let i = 0; i < L.count; i++) {
    // Not a junction connector, a footway (2) or a crossing (6): a closure closes a road.
    if (L.junctionId[i] !== 0xffffffff) continue;
    const kind = L.laneType[i];
    if (kind === 2 || kind === 6) continue;
    const off = L.pointOff[i];
    for (let k = 0; k + 1 < L.pointCount[i]; k++) {
      const ax = P.x[off + k];
      const ay = P.y[off + k];
      const bx = P.x[off + k + 1];
      const by = P.y[off + k + 1];
      const dx = bx - ax;
      const dy = by - ay;
      const len2 = dx * dx + dy * dy;
      const u = len2 > 0 ? Math.min(1, Math.max(0, ((x - ax) * dx + (y - ay) * dy) / len2)) : 0;
      const d = Math.hypot(ax + u * dx - x, ay + u * dy - y);
      if (best === null || d < best.distanceM) {
        best = { edge: L.edgeId[i], name: world.str(L.strName[i]), distanceM: d };
      }
    }
  }
  return best;
}

