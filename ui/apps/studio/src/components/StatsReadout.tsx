/**
 * The two frame-rate readouts, each as its own leaf.
 *
 * `stats` is the one slice of the store whose content genuinely changes on every 5 Hz flush — fps
 * and frame time move constantly in a live run. Subscribing to it from `App` or from `Viewport`
 * therefore re-renders those components (and reconciles every child they own: the scenario panel,
 * the inspector's ~50-row telemetry table, the plots strip) five times a second for the sake of
 * one number. These leaves subscribe instead, so the 5 Hz re-render is confined to a single
 * `<span>`. 09-ui §4 puts the HUD at 5 Hz; nothing says the whole tree has to follow.
 */

import { useStudio } from "../state/store.js";

/** The topbar readout: fps, frame time, drawn/live actor counts. */
export function StatsReadout(): React.JSX.Element | null {
  const stats = useStudio((s) => s.stats);
  if (!stats) return null;
  return (
    <span className="meta" data-testid="fps">
      {stats.fps.toFixed(0)} fps · {stats.frameMs.toFixed(1)} ms · {stats.actorInstances} drawn / {stats.actorLive} live
    </span>
  );
}

/** The viewport toolbar chip: fps and draw calls. */
export function StatsChip(): React.JSX.Element {
  const stats = useStudio((s) => s.stats);
  return (
    <span className="chip" data-testid="stats-chip">
      {stats ? `${stats.fps.toFixed(0)} fps · ${stats.drawCalls} calls` : "—"}
    </span>
  );
}
