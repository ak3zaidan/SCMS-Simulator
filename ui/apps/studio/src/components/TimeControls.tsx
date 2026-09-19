/**
 * The time controls of 09-ui §6: "play, pause, step (one mobility step or one event), speed
 * (0.1×–100×), scrub bar with event markers, jump to event".
 *
 * Every control is one JSON-RPC call from §6.6 and nothing else — `run.resume`, `run.pause`,
 * `run.step`, `run.speed`, `run.seek` — which is what makes the copilot's tool surface identical to
 * the UI's (09-ui §8). The scrub bar's markers are the event channels the connection is subscribed
 * to (§6.12 `events.set`), positioned by each event's `sim_time_ns`.
 *
 * The scrub bar commits on release, not on change. React maps `onChange` on an
 * `<input type="range">` onto the DOM `input` event, so a plain `onChange={seek}` fires once for
 * every step the thumb crosses: a single drag across a 300 s run at the reference 0.1 s mobility
 * step is 3,000 `run.seek` calls, each one pausing the run, each one followed by a `run.status`
 * poll. While the pointer (or an arrow key) is down the value is held in local state, which also
 * stops the 5 Hz stream from fighting the thumb; one `run.seek` goes out when the gesture ends.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { engine } from "../state/engine.js";
import { useStudio } from "../state/store.js";
import { simClock } from "../lib/format.js";

/** Speeds the selector offers, within the 0.1×–100× range 09-ui §6 asks for. */
const SPEEDS = [0.1, 0.25, 0.5, 1, 2, 5, 10, 25, 50, 100];

const MARK_COLOR: Record<string, string> = {
  "sec.cert": "var(--state-reported, #e69f00)",
  "det.observation": "var(--gt-tag, #009e73)",
  "proto.revocation": "var(--state-revoked, #cc79a7)",
  "app.warning": "var(--state-attacker, #d55e00)",
};

export function TimeControls(): React.JSX.Element {
  const run = useStudio((s) => s.run);
  const simTimeNs = useStudio((s) => s.simTimeNs);
  const timeline = useStudio((s) => s.timeline);
  const hello = useStudio((s) => s.hello);
  const connection = useStudio((s) => s.connection);
  const [stepUnit, setStepUnit] = useState<"step" | "keyframe" | "second">("step");
  const [busy, setBusy] = useState(false);
  /** The value under the thumb while a scrub gesture is in flight; `null` when it is not. */
  const [scrubNs, setScrubNs] = useState<number | null>(null);
  const scrubRef = useRef<number | null>(null);

  const endNs = run.tEndNs > 0 ? run.tEndNs : hello?.simDurationNs ?? 0;
  const streamNs = simTimeNs > 0 ? simTimeNs : run.tNs;
  // The clock, the fill and the thumb all read the dragged value, so the readout stays live while
  // the gesture is in flight and no seek has been issued yet.
  const nowNs = scrubNs ?? streamNs;
  const fraction = endNs > 0 ? Math.min(1, Math.max(0, nowNs / endNs)) : 0;
  const connected = connection === "streaming";

  const call = useCallback(async (fn: () => Promise<unknown>) => {
    setBusy(true);
    try {
      await fn();
    } catch {
      /* logged by engine.request */
    } finally {
      setBusy(false);
      void engine.refreshStatus();
    }
  }, []);

  const marks = useMemo(() => {
    if (endNs <= 0) return [];
    const seen = new Map<string, { left: number; channel: string; label: string; tNs: number }>();
    for (const m of timeline) {
      const left = Math.min(100, Math.max(0, (m.tNs / endNs) * 100));
      const key = `${m.channel}:${left.toFixed(2)}`;
      if (!seen.has(key)) seen.set(key, { left, channel: m.channel, label: m.label, tNs: m.tNs });
    }
    return [...seen.values()];
  }, [timeline, endNs]);

  const seekTo = useCallback(
    (tNs: number) => {
      void call(() => engine.request("run.seek", { t_ns: Math.round(tNs), pause_after: true }));
    },
    [call],
  );

  /**
   * End of gesture: issue the one `run.seek` the whole drag is worth.
   *
   * The thumb keeps showing where the user put it until `run.status` confirms the new time, so it
   * does not snap back to the pre-seek position for the length of the round trip.
   */
  const commitScrub = useCallback(async () => {
    const value = scrubRef.current;
    scrubRef.current = null;
    if (value === null) {
      setScrubNs(null);
      return;
    }
    setBusy(true);
    try {
      await engine.request("run.seek", { t_ns: Math.round(value), pause_after: true });
    } catch {
      /* logged by engine.request */
    } finally {
      setBusy(false);
      await engine.refreshStatus();
      setScrubNs(null);
    }
  }, []);

  // A range thumb dragged past the edge of the input releases the pointer somewhere else, so the
  // release is caught on the window rather than on the element.
  useEffect(() => {
    if (scrubNs === null) return;
    const onUp = (): void => void commitScrub();
    window.addEventListener("pointerup", onUp);
    window.addEventListener("pointercancel", onUp);
    return () => {
      window.removeEventListener("pointerup", onUp);
      window.removeEventListener("pointercancel", onUp);
    };
  }, [scrubNs, commitScrub]);

  return (
    <div className="timebar" data-testid="time-controls">
      <div className="transport">
        <button
          type="button"
          className="icon"
          title="Seek to the start (run.seek t_ns=0)"
          disabled={!connected || busy}
          onClick={() => seekTo(0)}
          data-testid="seek-start"
        >
          ◀◀
        </button>
        <button
          type="button"
          className="icon"
          title="Step back one mobility step — run.seek t_ns = now − Δt_mob"
          disabled={!connected || busy}
          onClick={() => seekTo(Math.max(0, nowNs - (hello?.mobilityStepNs ?? 1e8)))}
          data-testid="step-back"
        >
          ◀
        </button>
        {run.state === "running" ? (
          <button
            type="button"
            className="icon primary"
            title="run.pause"
            disabled={!connected || busy}
            onClick={() => void call(() => engine.request("run.pause", {}))}
            data-testid="pause"
          >
            ❚❚
          </button>
        ) : (
          <button
            type="button"
            className="icon primary"
            title="run.resume"
            disabled={!connected || busy}
            onClick={() => void call(() => engine.request("run.resume", {}))}
            data-testid="play"
          >
            ▶
          </button>
        )}
        <button
          type="button"
          className="icon"
          title={`run.step {unit: ${stepUnit}, count: 1}`}
          disabled={!connected || busy}
          onClick={() => void call(() => engine.request("run.step", { unit: stepUnit, count: 1 }))}
          data-testid="step"
        >
          ▶▶
        </button>
      </div>

      <select
        value={stepUnit}
        onChange={(e) => setStepUnit(e.target.value as "step" | "keyframe" | "second")}
        style={{ width: "auto" }}
        aria-label="Step unit"
        data-testid="step-unit"
      >
        <option value="step">step</option>
        <option value="keyframe">keyframe</option>
        <option value="second">second</option>
      </select>

      <select
        value={String(run.speed)}
        onChange={(e) => void call(() => engine.request("run.speed", { speed: Number(e.target.value) }))}
        style={{ width: "auto" }}
        aria-label="Speed"
        data-testid="speed"
        disabled={!connected}
      >
        {SPEEDS.map((s) => (
          <option key={s} value={String(s)}>
            {s}×
          </option>
        ))}
      </select>

      <div className="scrub" data-testid="scrub">
        <div className="track" />
        <div className="fill" style={{ width: `${fraction * 100}%` }} />
        {marks.map((m) => (
          <div
            key={`${m.channel}-${m.left}`}
            className="mark"
            style={{ left: `${m.left}%`, background: MARK_COLOR[m.channel] ?? "var(--accent)" }}
            title={`${m.channel} @ ${simClock(m.tNs)} — ${m.label}`}
          />
        ))}
        <input
          type="range"
          min={0}
          max={Math.max(1, endNs)}
          step={hello?.mobilityStepNs ?? 1e8}
          value={nowNs}
          disabled={!connected || endNs <= 0}
          aria-label="Scrub"
          aria-valuetext={simClock(nowNs)}
          data-testid="scrub-range"
          onPointerDown={() => {
            scrubRef.current = nowNs;
            setScrubNs(nowNs);
          }}
          onKeyDown={(e) => {
            // Arrow/Home/End move the thumb; the seek waits for the key to come back up, so
            // holding an arrow down is still one call.
            if (e.key.startsWith("Arrow") || e.key === "Home" || e.key === "End" || e.key === "PageUp" || e.key === "PageDown") {
              if (scrubRef.current === null) {
                scrubRef.current = nowNs;
                setScrubNs(nowNs);
              }
            }
          }}
          onKeyUp={() => {
            if (scrubRef.current !== null) void commitScrub();
          }}
          onChange={(e) => {
            const value = Number(e.target.value);
            if (scrubRef.current !== null) {
              scrubRef.current = value;
              setScrubNs(value);
            } else {
              // No gesture in flight (a programmatic change, or a click on the track that the
              // browser reported without a pointerdown): commit it directly.
              seekTo(value);
            }
          }}
          onBlur={() => {
            if (scrubRef.current !== null) void commitScrub();
          }}
        />
      </div>

      <span className="clock" data-testid="sim-clock">
        {simClock(nowNs)}
      </span>
      <span className="dim mono" title="run.status">
        / {simClock(endNs)} · {run.state}
      </span>
      {timeline.length > 0 ? (
        <button
          type="button"
          className="icon"
          title="Jump to the next event marker (run.seek)"
          disabled={!connected || busy}
          onClick={() => {
            const next = timeline.map((m) => m.tNs).filter((t) => t > nowNs).sort((a, b) => a - b)[0];
            if (next !== undefined) seekTo(next);
          }}
          data-testid="next-event"
        >
          ⤼ event
        </button>
      ) : null}
    </div>
  );
}
