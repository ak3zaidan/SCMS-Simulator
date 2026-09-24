/**
 * The scenario timeline, edited as a list of events rather than as a block of JSON.
 *
 * `events` is one setting of the scenario document (03-interfaces §13: `{t, until?, type, …}`),
 * and the generic form showed it as a JSON text box — correct and unusable. This editor keeps the
 * same document shape and edits it row by row: when, until when, what kind, and the one or two
 * parameters that kind takes. Applying the settings sends the list with everything else, and the
 * next run fires them.
 *
 * What each kind does in the engine is `crates/v2xw-engine/src/timeline.rs`; the parameters a
 * `param.change` may set are that file's `LIVE_PARAMS`, mirrored here so the choice is a list and
 * not a guess. A path outside that list is refused by the engine at Check or Apply, with the list.
 */

import { useCallback, useMemo, useState } from "react";

import { engine } from "../state/engine.js";
import { useStudio } from "../state/store.js";

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

const WEATHER = ["clear", "rain", "snow", "sleet", "fog", "wind"] as const;

/** The first item of a kind, with the parameters it needs filled with something sensible. */
function newItem(type: string, t: number): EventItem {
  switch (type) {
    case "closure":
      return { t, type, target: "" };
    case "demand.multiplier":
      return { t, type, value: 2 };
    case "weather.front":
      return { t, type, value: "fog" };
    case "param.change":
      return { t, type, path: LIVE_PARAMS[0].path, value: "rain" };
    case "outage":
      return { t, type, target: 0 };
    default:
      return { t, type, ids: [0] };
  }
}

/** The nearest vehicle lane's edge to a map point, with its street name when it has one. */
export function nearestEdge(
  world: NonNullable<typeof engine.world>,
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

function num(v: string): number | undefined {
  if (v.trim() === "") return undefined;
  const n = Number(v);
  return Number.isFinite(n) ? n : undefined;
}

/** A parameter value typed as text: a number when it reads as one, JSON when it parses, else a string. */
function looseValue(text: string): unknown {
  const n = num(text);
  if (n !== undefined) return n;
  try {
    return JSON.parse(text) as unknown;
  } catch {
    return text;
  }
}

export function EventsEditor({
  value,
  onChange,
  durationS,
}: {
  value: unknown;
  onChange: (next: EventItem[]) => void;
  durationS: number | undefined;
}): React.JSX.Element {
  const items: EventItem[] = useMemo(
    () => (Array.isArray(value) ? (value as EventItem[]) : []),
    [value],
  );
  const fired = useStudio((s) => s.firedEvents);
  const picking = useStudio((s) => s.mapPick);
  const [pickNote, setPickNote] = useState<string | null>(null);

  const update = useCallback(
    (index: number, patch: Partial<EventItem> | null) => {
      const next = items.map((it) => ({ ...it }));
      if (patch === null) next.splice(index, 1);
      else next[index] = { ...next[index], ...patch } as EventItem;
      onChange(next);
    },
    [items, onChange],
  );

  const replaceKind = useCallback(
    (index: number, type: string) => {
      const next = items.map((it) => ({ ...it }));
      const fresh = newItem(type, next[index].t);
      const kind = EVENT_KINDS.find((k) => k.type === type);
      if (kind?.ends && next[index].until !== undefined) fresh.until = next[index].until;
      next[index] = fresh;
      onChange(next);
    },
    [items, onChange],
  );

  const pickOnMap = useCallback(
    (index: number) => {
      setPickNote("Click a road on the map to close it.");
      useStudio.getState().setMapPick({
        purpose: "closure",
        resolve: ({ x, y }) => {
          const world = engine.world;
          const hit = world ? nearestEdge(world, x, y) : null;
          if (hit === null) {
            setPickNote("No road near that point.");
            return;
          }
          update(index, { target: `edge:${hit.edge}` });
          setPickNote(
            `Picked edge ${hit.edge}${hit.name ? ` (${hit.name})` : ""}, ${hit.distanceM.toFixed(0)} m from the click.`,
          );
        },
      });
    },
    [update],
  );

  return (
    <div className="events-editor" data-testid="events-editor">
      {items.length === 0 ? (
        <p className="help">No events: the run plays out with the settings it starts with.</p>
      ) : null}
      {items.map((it, i) => {
        const kind = EVENT_KINDS.find((k) => k.type === it.type);
        const done = fired.filter((f) => f.index === i);
        return (
          <div className="event-row" key={i} data-testid="event-row" data-index={i}>
            <div className="row">
              <select
                aria-label="What happens"
                value={it.type}
                data-testid="event-type"
                onChange={(e) => replaceKind(i, e.target.value)}
              >
                {EVENT_KINDS.map((k) => (
                  <option key={k.type} value={k.type}>
                    {k.label}
                  </option>
                ))}
              </select>
              <label className="inline">
                at
                <input
                  type="number"
                  min={0}
                  max={durationS}
                  step="any"
                  value={it.t}
                  data-testid="event-t"
                  aria-label="When, seconds of simulated time"
                  onChange={(e) => update(i, { t: num(e.target.value) ?? 0 })}
                />
                s
              </label>
              {kind?.ends ? (
                <label className="inline">
                  until
                  <input
                    type="number"
                    min={0}
                    max={durationS}
                    step="any"
                    value={it.until ?? ""}
                    placeholder="the end"
                    data-testid="event-until"
                    aria-label="Until when, seconds; empty means to the end of the run"
                    onChange={(e) => {
                      const until = num(e.target.value);
                      const next = { ...it } as EventItem;
                      if (until === undefined) delete next.until;
                      else next.until = until;
                      const all = items.map((x) => ({ ...x }));
                      all[i] = next;
                      onChange(all);
                    }}
                  />
                  s
                </label>
              ) : null}
              <span className="grow" />
              <button type="button" className="linklike" data-testid="event-remove" onClick={() => update(i, null)} aria-label="Remove this event">
                remove
              </button>
            </div>

            {it.type === "closure" ? (
              <div className="row">
                <input
                  type="text"
                  value={String(it.target ?? "")}
                  placeholder="edge:12, lane:340 or street:West 42nd Street"
                  data-testid="event-target"
                  aria-label="Which road"
                  onChange={(e) => update(i, { target: e.target.value })}
                />
                <button
                  type="button"
                  data-testid="event-pick"
                  className={picking?.purpose === "closure" ? "active" : undefined}
                  title="Pick the road by clicking it on the map"
                  onClick={() => pickOnMap(i)}
                >
                  pick on map
                </button>
              </div>
            ) : null}
            {it.type === "demand.multiplier" ? (
              <label className="inline">
                × arrivals
                <input
                  type="number"
                  min={0}
                  step="any"
                  value={typeof it.value === "number" ? it.value : ""}
                  data-testid="event-value"
                  onChange={(e) => update(i, { value: num(e.target.value) ?? 1 })}
                />
              </label>
            ) : null}
            {it.type === "weather.front" ? (
              <div className="row">
                <select value={String(it.value ?? "fog")} data-testid="event-value" aria-label="Weather" onChange={(e) => update(i, { value: e.target.value })}>
                  {WEATHER.map((w) => (
                    <option key={w} value={w}>
                      {w}
                    </option>
                  ))}
                </select>
                <label className="inline">
                  intensity
                  <input
                    type="number"
                    min={0}
                    max={1}
                    step={0.1}
                    value={typeof it.intensity === "number" ? it.intensity : 1}
                    onChange={(e) => update(i, { intensity: num(e.target.value) ?? 1 })}
                  />
                </label>
              </div>
            ) : null}
            {it.type === "param.change" ? (
              <div className="row">
                <select value={String(it.path ?? "")} data-testid="event-path" aria-label="Which setting" onChange={(e) => update(i, { path: e.target.value })}>
                  {LIVE_PARAMS.some((p) => p.path === it.path) ? null : <option value={String(it.path ?? "")}>{String(it.path ?? "—")}</option>}
                  {LIVE_PARAMS.map((p) => (
                    <option key={p.path} value={p.path} title={`reaches ${p.reach}`}>
                      {p.path}
                    </option>
                  ))}
                </select>
                <input
                  type="text"
                  value={typeof it.value === "string" ? it.value : JSON.stringify(it.value ?? "")}
                  data-testid="event-value"
                  aria-label="New value"
                  onChange={(e) => update(i, { value: looseValue(e.target.value) })}
                />
              </div>
            ) : null}
            {it.type === "outage" ? (
              <label className="inline">
                node
                <input
                  type="number"
                  min={0}
                  step={1}
                  value={typeof it.target === "number" ? it.target : ""}
                  data-testid="event-target"
                  onChange={(e) => update(i, { target: num(e.target.value) ?? 0 })}
                />
              </label>
            ) : null}
            {it.type === "attack.wave" ? (
              <label className="inline">
                populations
                <input
                  type="text"
                  value={Array.isArray(it.ids) ? (it.ids as unknown[]).join(", ") : ""}
                  placeholder="0, 1 — or an attacker id"
                  data-testid="event-ids"
                  onChange={(e) =>
                    update(i, {
                      ids: e.target.value
                        .split(",")
                        .map((x) => x.trim())
                        .filter((x) => x !== "")
                        .map((x) => num(x) ?? x),
                    })
                  }
                />
              </label>
            ) : null}
            <div className="help">{kind?.help ?? `The engine does not know '${it.type}'.`}</div>
            {done.map((f) => (
              <div className="help fired" data-testid="event-fired" key={`${f.phase}-${f.t}`}>
                {f.phase === "end" ? "Ended" : "Fired"} at {(f.t / 1e9).toFixed(1)} s: {f.effect}
              </div>
            ))}
          </div>
        );
      })}
      <div className="row">
        <select
          data-testid="event-add"
          value=""
          aria-label="Add an event"
          onChange={(e) => {
            if (e.target.value === "") return;
            onChange([...items.map((x) => ({ ...x })), newItem(e.target.value, 0)]);
          }}
        >
          <option value="">Add an event…</option>
          {EVENT_KINDS.map((k) => (
            <option key={k.type} value={k.type}>
              {k.label}
            </option>
          ))}
        </select>
      </div>
      {pickNote ? (
        <div className="note info" data-testid="event-pick-note">
          {pickNote}
        </div>
      ) : null}
    </div>
  );
}
