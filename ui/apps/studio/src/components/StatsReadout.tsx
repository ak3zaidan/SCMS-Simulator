/**
 * The two frame-rate readouts, each as its own leaf.
 *
 * `stats` is the one slice of the store whose content genuinely changes on every 5 Hz flush — fps
 * and frame time move constantly in a live run. Subscribing to it from `App` or from `Viewport`
 * therefore re-renders those components (and reconciles every child they own: the scenario panel,
 * the inspector's ~50-row telemetry table, the plots strip) five times a second for the sake of
 * one number. These leaves subscribe instead, so the 5 Hz re-render is confined to a single
 * `<span>`. 09-ui §4 puts the HUD at 5 Hz; nothing says the whole tree has to follow.
 *
 * Every figure here is a button, for the same reason every HUD field is: these are the numbers a
 * reader is most likely to quote out of context, and they are the ones the engine has never seen.
 * `lib/provenance.ts` says what measured them, from what, and what they do not mean — a
 * presentation rate is not a simulation rate, and `actorInstances` below `actorLive` is culling
 * rather than actors leaving the run.
 */

import { useStudio } from "../state/store.js";
import { clientSubject } from "../lib/provenance.js";

/** One explainable figure. `unit` is the wire-free unit of the client-side measurement. */
function Stat({ id, label, text, unit }: { id: string; label: string; text: string; unit: string }): React.JSX.Element {
  const setWhy = useStudio((s) => s.setWhy);
  return (
    <button
      type="button"
      className="stat"
      data-testid={`stat-${id}`}
      aria-label={`${label}: ${text} ${unit} — explain`}
      onClick={() => setWhy(clientSubject(id, label, text, unit))}
    >
      {text}
    </button>
  );
}

/** The topbar readout: fps, frame time, drawn/live actor counts. */
export function StatsReadout(): React.JSX.Element | null {
  const stats = useStudio((s) => s.stats);
  if (!stats) return null;
  return (
    <span className="meta" data-testid="fps">
      <Stat id="fps" label="frames per second" text={stats.fps.toFixed(0)} unit="1/s" /> fps ·{" "}
      <Stat id="frameMs" label="frame time" text={stats.frameMs.toFixed(1)} unit="ms" /> ms ·{" "}
      <Stat id="actorInstances" label="actor instances drawn" text={String(stats.actorInstances)} unit="count" /> drawn /{" "}
      <Stat id="actorLive" label="live actors" text={String(stats.actorLive)} unit="count" /> live
    </span>
  );
}

/** The viewport toolbar chip: fps and draw calls. */
export function StatsChip(): React.JSX.Element {
  const stats = useStudio((s) => s.stats);
  if (!stats) {
    return (
      <span className="chip" data-testid="stats-chip">
        —
      </span>
    );
  }
  return (
    <span className="chip" data-testid="stats-chip">
      <Stat id="fps" label="frames per second" text={stats.fps.toFixed(0)} unit="1/s" /> fps ·{" "}
      <Stat id="drawCalls" label="draw calls" text={String(stats.drawCalls)} unit="count" /> calls ·{" "}
      <Stat id="p95Ms" label="frame time p95" text={stats.p95Ms.toFixed(1)} unit="ms" /> p95
    </span>
  );
}
