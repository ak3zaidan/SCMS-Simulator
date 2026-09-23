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
 *
 * Two additions to that sketch:
 *
 *  * **A recording takes the bar over.** With a local recording open (09-ui §7) the scrub drives the
 *    WebAssembly reader instead of `run.seek`, over the recording's own span, and the transport —
 *    play, pause, step, speed — is disabled, because a recording has none of those: it has a seek.
 *  * **Side B follows.** While the comparison view is synchronised, every control that moves side
 *    A's clock moves side B's afterwards, never in parallel (§6.6 R4 puts a seek's frames before its
 *    reply, and two in flight would interleave).
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { compare } from "../state/compare.js";
import { engine } from "../state/engine.js";
import { useStudio } from "../state/store.js";
import { simClock } from "../lib/format.js";
import { eventSubject } from "../lib/provenance.js";

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
  const replay = useStudio((s) => s.replay);
  const compareSide = useStudio((s) => s.compare);
  const compareSync = useStudio((s) => s.compareSync);
  const setWhy = useStudio((s) => s.setWhy);
  const [stepUnit, setStepUnit] = useState<"step" | "keyframe" | "second">("step");
  const [busy, setBusy] = useState(false);
  /** The value under the thumb while a scrub gesture is in flight; `null` when it is not. */
  const [scrubNs, setScrubNs] = useState<number | null>(null);
  const [eventsOpen, setEventsOpen] = useState(false);
  const scrubRef = useRef<number | null>(null);

  /**
   * Which clock the bar is driving.
   *
   * A local recording wins over a connection whenever one is open, because that is also what the
   * viewport is showing: `StudioEngine.openLocalRecording` detaches the stream from the viewer, so
   * a bar that kept issuing `run.seek` would move a run nobody can see. §7.3 makes the recording's
   * own seek the cheaper of the two anyway — one chunk and one keyframe period of deltas.
   */
  const drivingReplay = replay !== null;
  const endNs = drivingReplay ? replay.endNs : run.tEndNs > 0 ? run.tEndNs : hello?.simDurationNs ?? 0;
  const startNs = drivingReplay ? replay.startNs : 0;
  const streamNs = drivingReplay ? replay.tNs : simTimeNs > 0 ? simTimeNs : run.tNs;
  // The clock, the fill and the thumb all read the dragged value, so the readout stays live while
  // the gesture is in flight and no seek has been issued yet.
  const nowNs = scrubNs ?? streamNs;
  const fraction = endNs > startNs ? Math.min(1, Math.max(0, (nowNs - startNs) / (endNs - startNs))) : 0;
  const connected = connection === "streaming" || drivingReplay;
  /** The transport — play, pause, step, speed — exists only for a run. A recording has none. */
  const transportLive = connection === "streaming" && !drivingReplay;

  const call = useCallback(
    async (fn: () => Promise<unknown>) => {
      setBusy(true);
      try {
        await fn();
      } catch {
        /* logged by engine.request */
      } finally {
        setBusy(false);
        if (!drivingReplay) await engine.refreshStatus();
        // A step, a pause or a resume moves side A's clock too, so side B follows it here rather
        // than only on a scrub: `run.status` has just been refreshed, so this reads the new time.
        if (compareSide !== null && compareSync.time) {
          const state = useStudio.getState();
          await compare.seekTo(state.simTimeNs > 0 ? state.simTimeNs : state.run.tNs);
        }
      }
    },
    [compareSide, compareSync.time, drivingReplay],
  );

  const marks = useMemo(() => {
    if (endNs <= startNs) return [];
    const seen = new Map<string, { left: number; channel: string; label: string; tNs: number }>();
    for (const m of timeline) {
      const left = Math.min(100, Math.max(0, ((m.tNs - startNs) / (endNs - startNs)) * 100));
      const key = `${m.channel}:${left.toFixed(2)}`;
      if (!seen.has(key)) seen.set(key, { left, channel: m.channel, label: m.label, tNs: m.tNs });
    }
    return [...seen.values()];
  }, [timeline, endNs, startNs]);

  /**
   * Whether a seek also moves the comparison side.
   *
   * The two runs share one simulated clock while `compareSync.time` is on, which is what "two runs
   * side by side with synchronised time" means (09-ui §6). Side B is always moved *after* side A,
   * never in parallel: a `run.seek` streams its keyframe and deltas before its reply (§6.6 R4), so
   * two of them in flight on one main thread would interleave their frames.
   *
   * Every transport control routes through {@link call}, which does that follow-up once. `seekTo`
   * therefore issues side A's seek only — issuing B's here as well would seek a recording twice for
   * one gesture.
   */
  const syncB = compareSide !== null && compareSync.time;

  const seekOne = useCallback(
    async (tNs: number): Promise<void> => {
      const target = Math.round(tNs);
      if (drivingReplay) {
        await engine.seekLocalReplay(target);
        return;
      }
      await engine.request("run.seek", { t_ns: target, pause_after: true });
    },
    [drivingReplay],
  );

  const seekTo = useCallback(
    (tNs: number) => {
      void call(() => seekOne(tNs));
    },
    [call, seekOne],
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
      await seekOne(value);
      if (compareSide !== null && compareSync.time) await compare.seekTo(Math.round(value));
    } catch {
      /* logged by engine.request */
    } finally {
      setBusy(false);
      if (!drivingReplay) await engine.refreshStatus();
      setScrubNs(null);
    }
  }, [compareSide, compareSync.time, seekOne, drivingReplay]);

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
          title="Seek to the start of the span"
          disabled={!connected || busy}
          onClick={() => seekTo(startNs)}
          data-testid="seek-start"
        >
          ◀◀
        </button>
        <button
          type="button"
          className="icon"
          title="Step back one mobility step — run.seek t_ns = now − Δt_mob"
          disabled={!connected || busy}
          onClick={() => seekTo(Math.max(startNs, nowNs - (hello?.mobilityStepNs ?? 1e8)))}
          data-testid="step-back"
        >
          ◀
        </button>
        {run.state === "running" ? (
          <button
            type="button"
            className="icon primary"
            title="run.pause"
            disabled={!transportLive || busy}
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
            disabled={!transportLive || busy}
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
          disabled={!transportLive || busy}
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
        disabled={!transportLive}
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
        {/*
          Decorative: the range input sits above the track and owns every pointer event in this
          box, so a marker cannot be clicked however it is marked up. The same events are reachable
          by keyboard — and explainable — through the `events ▾` list at the end of the bar, which
          is the accessible surface for them (09-ui §10) rather than a focusable element that
          cannot be activated with a pointer.
        */}
        {marks.map((m) => (
          <div
            key={`${m.channel}-${m.left}`}
            className="mark"
            aria-hidden="true"
            style={{ left: `${m.left}%`, background: MARK_COLOR[m.channel] ?? "var(--accent)" }}
            title={`${m.channel} @ ${simClock(m.tNs)} — ${m.label}`}
          />
        ))}
        <input
          type="range"
          min={startNs}
          max={Math.max(startNs + 1, endNs)}
          step={hello?.mobilityStepNs ?? 1e8}
          value={nowNs}
          disabled={!connected || endNs <= startNs}
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
      <span className="dim mono" title={drivingReplay ? "the recording's own span (§7.3)" : "run.status"}>
        / {simClock(endNs)} · {drivingReplay ? "recording" : run.state}
      </span>
      {syncB ? (
        <span className="chip" data-testid="sync-chip" title="A scrub seeks both runs; see the Compare panel">
          B synced{compareSync.offsetNs === 0 ? "" : ` ${(compareSync.offsetNs / 1e9).toFixed(1)} s`}
        </span>
      ) : null}

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

      {timeline.length > 0 ? (
        <div className="menu">
          <button
            type="button"
            className="icon"
            onClick={() => setEventsOpen((v) => !v)}
            aria-expanded={eventsOpen}
            data-testid="event-list-button"
          >
            events ▾ <span className="dim">{timeline.length}</span>
          </button>
          {eventsOpen ? (
            <div className="menu-pop up wide" data-testid="event-list">
              <div className="sec">Latest markers — activate to seek, or open the provenance</div>
              {[...timeline]
                .slice(-25)
                .reverse()
                .map((m, i) => (
                  <div className="row" key={`${m.tNs}-${m.channel}-${m.nodeId}-${i}`}>
                    <button
                      type="button"
                      className="linklike"
                      disabled={!connected || busy}
                      onClick={() => seekTo(m.tNs)}
                      aria-label={`Seek to ${simClock(m.tNs)} — ${m.channel}, ${m.label}`}
                    >
                      {simClock(m.tNs)}
                    </button>
                    <span className="dim" style={{ color: MARK_COLOR[m.channel] ?? "var(--accent)" }}>
                      {m.channel}
                    </span>
                    <button
                      type="button"
                      className="linklike grow"
                      onClick={() => setWhy(eventSubject(m.channel, m.label, m.nodeId, m.provId))}
                      aria-label={`Explain ${m.label} on node ${m.nodeId}`}
                    >
                      {m.label}
                    </button>
                  </div>
                ))}
            </div>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}
