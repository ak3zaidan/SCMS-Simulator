/**
 * The transport bar: where the run is, what it is doing, and the five controls that move it.
 *
 * Every control is one JSON-RPC call — `run.resume`, `run.pause`, `run.step`, `run.speed`,
 * `run.seek`, `run.start` — which is what makes the copilot's tool surface identical to the UI's.
 * What changed is that the bar now tells the truth about all six.
 *
 * # What it used to say
 *
 * `00:00:00.000 / 00:01:00.000 · idle`, against a server reporting `state: "finished"` at 60.1
 * seconds. Three errors in one line, all the same error: the 2 s `run.status` poll went through the
 * socket, the socket closes when a run ends, the poll failed, and the store kept the zeroes it was
 * initialised with. The bar reported the absence of an answer as a fact about the run. The poll now
 * falls back to HTTP (`StudioEngine.request`), so those figures are the engine's.
 *
 * # What it says now
 *
 *  * **The state in words**, from the same description the header chip reads (`lib/status.ts`), so
 *    the two cannot disagree. Never a wire token: `idle` is not a word about a simulation.
 *  * **A disabled control says why.** Which controls the engine will accept depends on the state it
 *    is in, and `state/transport.ts` holds those rules with the engine's own refusals beside them.
 *    A greyed button with no tooltip is a dead end; "Pause first — the engine refuses a step while
 *    the run is moving" is an instruction.
 *  * **How much of the span exists.** The bar draws the part of the run that has been simulated
 *    separately from the part that has not, because dragging into the second one is refused.
 *  * **A refused seek is reported.** `run.seek` outside the produced range fails with `-32003`,
 *    whose `data` carries the range that *would* have worked. That used to be swallowed by a bare
 *    `catch`, so the thumb snapped back and nothing was said. It is now shown, and the range it
 *    reports is remembered and drawn.
 *  * **Restart is here**, not only in the header, because this is the bar you are looking at when a
 *    run ends. It rewinds, reopens the closed stream and resumes, in that order
 *    (`StudioEngine.startRun`).
 *
 * The scrub bar commits on release, not on change. React maps `onChange` on an
 * `<input type="range">` onto the DOM `input` event, so a plain `onChange={seek}` fires once for
 * every step the thumb crosses: a single drag across a 300 s run at the reference 0.1 s mobility
 * step is 3,000 `run.seek` calls, each one pausing the run, each one followed by a `run.status`
 * poll. While the pointer (or an arrow key) is down the value is held in local state, which also
 * stops the 5 Hz stream from fighting the thumb; one `run.seek` goes out when the gesture ends.
 *
 * Two further cases the bar carries:
 *
 *  * **A recording takes it over.** With a local recording open the scrub drives the WebAssembly
 *    reader instead of `run.seek`, over the recording's own span, and the transport is not merely
 *    disabled but gone: a recording has no clock to start, and an inert Play button invites a press
 *    that can never do anything.
 *  * **Side B follows.** While the comparison view is synchronised, every control that moves side
 *    A's clock moves side B's afterwards, never in parallel (a seek's frames precede its reply, and
 *    two in flight would interleave).
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { JsonRpcError } from "@vwp/protocol";

import { compare } from "../state/compare.js";
import { engine } from "../state/engine.js";
import { useStudio } from "../state/store.js";
import { transport as transportCaps } from "../state/transport.js";
import { durationNs, simClock } from "../lib/format.js";
import { eventSubject } from "../lib/provenance.js";
import { useStatus } from "./Status.js";

/**
 * Speeds the selector offers.
 *
 * `0` is in the list because the engine accepts it and means something useful by it — the producer
 * skips its wall-clock sleep entirely — and because it is how anyone running a batch actually wants
 * to run: a 60 s scenario at 1× takes a minute of your life for no reason. The selector offered
 * 0.1× to 100× and left the fastest setting the server has reachable only from a terminal.
 */
const SPEEDS = [0, 0.1, 0.25, 0.5, 1, 2, 5, 10, 25, 50, 100];

function speedLabel(speed: number): string {
  return speed === 0 ? "as fast as possible" : `${speed}×`;
}

/** What one press of Step advances, in words. */
const STEP_UNITS: readonly { value: "step" | "keyframe" | "second"; label: string }[] = [
  { value: "step", label: "one step" },
  { value: "keyframe", label: "one keyframe" },
  { value: "second", label: "one second" },
];

const MARK_COLOR: Record<string, string> = {
  "sec.cert": "var(--state-reported, #e69f00)",
  "det.observation": "var(--gt-tag, #009e73)",
  "proto.revocation": "var(--state-revoked, #cc79a7)",
  "app.warning": "var(--state-attacker, #d55e00)",
};

/** What each event channel is, so a marker's tooltip is a sentence and not a wire name. */
const CHANNEL_LABEL: Record<string, string> = {
  "sec.cert": "certificate change",
  "det.observation": "misbehaviour observation",
  "proto.revocation": "revocation",
  "app.warning": "safety warning",
};

/** The seekable range an engine reported when it refused a seek (`-32003`). */
interface SeekRefusal {
  readonly minNs: number;
  readonly maxNs: number;
}

/** Read `{min_ns, max_ns}` out of a `-32003` error, if that is what this is. */
function seekRefusal(err: unknown): SeekRefusal | null {
  if (!(err instanceof JsonRpcError)) return null;
  const data = err.data;
  if (data === null || typeof data !== "object") return null;
  const min = (data as { min_ns?: unknown }).min_ns;
  const max = (data as { max_ns?: unknown }).max_ns;
  if (typeof min !== "number" || typeof max !== "number") return null;
  return { minNs: min, maxNs: max };
}

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
  const status = useStatus();
  const [stepUnit, setStepUnit] = useState<"step" | "keyframe" | "second">("step");
  const [busy, setBusy] = useState(false);
  /** The value under the thumb while a scrub gesture is in flight; `null` when it is not. */
  const [scrubNs, setScrubNs] = useState<number | null>(null);
  const [eventsOpen, setEventsOpen] = useState(false);
  /** The last thing a control said, when it was not what the user asked for. */
  const [notice, setNotice] = useState<string | null>(null);
  /** The range the engine reported the last time it refused a seek. */
  const [refused, setRefused] = useState<SeekRefusal | null>(null);
  const scrubRef = useRef<number | null>(null);

  /**
   * Which clock the bar is driving.
   *
   * A local recording wins over a connection whenever one is open, because that is also what the
   * viewport is showing: `StudioEngine.openLocalRecording` detaches the stream from the viewer, so
   * a bar that kept issuing `run.seek` would move a run nobody can see. The recording's own seek is
   * the cheaper of the two anyway — one chunk and one keyframe period of deltas.
   */
  const drivingReplay = replay !== null;
  const streamNs = drivingReplay ? replay.tNs : simTimeNs > 0 ? simTimeNs : run.tNs;

  const caps = useMemo(
    () =>
      transportCaps({
        connection,
        runState: run.state,
        tNs: run.tNs,
        tEndNs: run.tEndNs > 0 ? run.tEndNs : hello?.simDurationNs ?? 0,
        streamNs,
        recording: replay === null ? null : { startNs: replay.startNs, endNs: replay.endNs },
        busy,
      }),
    [connection, run.state, run.tNs, run.tEndNs, hello?.simDurationNs, streamNs, replay, busy],
  );

  const { minNs: startNs, spanNs: endNs } = caps;
  // The clock, the fill and the thumb all read the dragged value, so the readout stays live while
  // the gesture is in flight and no seek has been issued yet.
  const nowNs = scrubNs ?? streamNs;
  const span = endNs - startNs;
  const pct = useCallback(
    (tNs: number) => (span > 0 ? Math.min(100, Math.max(0, ((tNs - startNs) / span) * 100)) : 0),
    [span, startNs],
  );
  // What the engine will actually seek to: what it has produced, narrowed to whatever a refusal
  // reported. Before any refusal this is only a hint, which is why it is drawn and not enforced.
  const seekableNs = refused === null ? caps.seekMaxNs : Math.min(caps.seekMaxNs, refused.maxNs);
  const showSeekable = caps.partial && seekableNs > startNs;

  const call = useCallback(
    async (fn: () => Promise<unknown>) => {
      setBusy(true);
      setNotice(null);
      try {
        await fn();
      } catch (err) {
        setNotice(err instanceof Error ? err.message : String(err));
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
    if (span <= 0) return [];
    const seen = new Map<string, { left: number; channel: string; label: string; tNs: number }>();
    for (const m of timeline) {
      const left = Math.min(100, Math.max(0, ((m.tNs - startNs) / span) * 100));
      const key = `${m.channel}:${left.toFixed(2)}`;
      if (!seen.has(key)) seen.set(key, { left, channel: m.channel, label: m.label, tNs: m.tNs });
    }
    return [...seen.values()];
  }, [timeline, span, startNs]);

  /**
   * Whether a seek also moves the comparison side.
   *
   * The two runs share one simulated clock while `compareSync.time` is on. Side B is always moved
   * *after* side A, never in parallel: a `run.seek` streams its keyframe and deltas before its
   * reply, so two of them in flight on one main thread would interleave their frames.
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
      try {
        await engine.request("run.seek", { t_ns: target, pause_after: true });
      } catch (err) {
        // §6.6's `-32003` carries the range that would have worked. Remembering it is how the bar
        // learns a bound `run.status` never publishes — and how the two engines in this repository,
        // which disagree about how far a run may be seeked, both end up drawn correctly.
        const range = seekRefusal(err);
        if (range === null) throw err;
        setRefused(range);
        throw new Error(
          `The engine has only simulated up to ${simClock(range.maxNs)} so far, so it cannot move to ` +
            `${simClock(target)} yet. Let the run reach that point first.`,
        );
      }
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
    setNotice(null);
    try {
      await seekOne(value);
      if (compareSide !== null && compareSync.time) await compare.seekTo(Math.round(value));
    } catch (err) {
      setNotice(err instanceof Error ? err.message : String(err));
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

  // A new run clears what the old one refused: `run.start` rewinds, and the range it will accept
  // grows again from zero.
  useEffect(() => {
    if (run.state === "idle" || run.tNs === 0) setRefused(null);
  }, [run.state, run.tNs]);

  return (
    <div className="timebar" data-testid="time-controls">
      <div className="transport">
        <button
          type="button"
          className={run.state === "finished" ? "icon primary" : "icon"}
          title={caps.restart.why}
          disabled={!caps.restart.enabled}
          onClick={() => void call(() => engine.startRun())}
          data-testid="restart"
          aria-label="Restart the run from the beginning"
        >
          ↺
        </button>
        <button
          type="button"
          className="icon"
          title={caps.seek.enabled ? "Go back to the start of the span" : caps.seek.why}
          disabled={!caps.seek.enabled}
          onClick={() => seekTo(startNs)}
          data-testid="seek-start"
          aria-label="Go to the start"
        >
          ◀◀
        </button>
        <button
          type="button"
          className="icon"
          title={caps.seek.enabled ? "Go back one mobility step" : caps.seek.why}
          disabled={!caps.seek.enabled}
          onClick={() => seekTo(Math.max(startNs, nowNs - (hello?.mobilityStepNs ?? 1e8)))}
          data-testid="step-back"
          aria-label="Back one step"
        >
          ◀
        </button>
        {run.state === "running" ? (
          <button
            type="button"
            className="icon primary"
            title={caps.pause.why}
            disabled={!caps.pause.enabled}
            onClick={() => void call(() => engine.request("run.pause", {}))}
            data-testid="pause"
            aria-label="Pause"
          >
            ❚❚
          </button>
        ) : (
          <button
            type="button"
            // Not `primary` while it is disabled: an accented button reads as the thing to press,
            // and on a finished run the thing to press is Restart.
            className={caps.play.enabled ? "icon primary" : "icon"}
            title={caps.play.why}
            disabled={!caps.play.enabled}
            onClick={() => void call(() => engine.request("run.resume", {}))}
            data-testid="play"
            aria-label="Play"
          >
            ▶
          </button>
        )}
        <button
          type="button"
          className="icon"
          title={caps.step.enabled ? `Advance ${STEP_UNITS.find((u) => u.value === stepUnit)?.label ?? "one step"} and stop` : caps.step.why}
          disabled={!caps.step.enabled}
          onClick={() => void call(() => engine.request("run.step", { unit: stepUnit, count: 1 }))}
          data-testid="step"
          aria-label="Step forward"
        >
          ▶▶
        </button>
        <button
          type="button"
          className="icon"
          title={caps.stop.why}
          disabled={!caps.stop.enabled}
          onClick={() => void call(() => engine.request("run.stop", {}))}
          data-testid="stop"
          aria-label="Stop the run"
        >
          ■
        </button>
      </div>

      <select
        value={stepUnit}
        onChange={(e) => setStepUnit(e.target.value as "step" | "keyframe" | "second")}
        style={{ width: "auto" }}
        aria-label="How far one press of Step advances"
        title="How far one press of Step advances"
        data-testid="step-unit"
      >
        {STEP_UNITS.map((u) => (
          <option key={u.value} value={u.value}>
            {u.label}
          </option>
        ))}
      </select>

      <select
        value={String(run.speed)}
        onChange={(e) => void call(() => engine.request("run.speed", { speed: Number(e.target.value) }))}
        style={{ width: "auto" }}
        aria-label="Speed"
        title={caps.speed.why}
        data-testid="speed"
        disabled={!caps.speed.enabled}
      >
        {(SPEEDS.includes(run.speed) ? SPEEDS : [run.speed, ...SPEEDS]).map((s) => (
          <option key={s} value={String(s)}>
            {speedLabel(s)}
          </option>
        ))}
      </select>

      <div className="scrub" data-testid="scrub">
        <div className="track" />
        {/*
          How much of the span exists. Drawn only when part of it does not: on a finished run, or
          one whose whole span has been produced, a second region would be a distinction without a
          difference.
        */}
        {showSeekable ? (
          <div
            className="produced"
            style={{ width: `${pct(seekableNs)}%` }}
            title={`Simulated up to ${simClock(seekableNs)}. Beyond that there is nothing to show yet.`}
          />
        ) : null}
        <div className="fill" style={{ width: `${pct(nowNs)}%` }} />
        {/*
          Decorative: the range input sits above the track and owns every pointer event in this
          box, so a marker cannot be clicked however it is marked up. The same events are reachable
          by keyboard — and explainable — through the `events ▾` list at the end of the bar, which
          is the accessible surface for them rather than a focusable element that cannot be
          activated with a pointer.
        */}
        {marks.map((m) => (
          <div
            key={`${m.channel}-${m.left}`}
            className="mark"
            aria-hidden="true"
            style={{ left: `${m.left}%`, background: MARK_COLOR[m.channel] ?? "var(--accent)" }}
            title={`${CHANNEL_LABEL[m.channel] ?? m.channel} at ${simClock(m.tNs)} — ${m.label}`}
          />
        ))}
        <input
          type="range"
          min={startNs}
          max={Math.max(startNs + 1, endNs)}
          step={hello?.mobilityStepNs ?? 1e8}
          value={nowNs}
          disabled={!caps.seek.enabled}
          aria-label="Position in simulated time"
          aria-valuetext={simClock(nowNs)}
          title={caps.seek.why}
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
      <span
        className="dim"
        data-testid="time-state"
        title={
          drivingReplay
            ? "The recording's own span"
            : `Simulated time, out of the ${durationNs(endNs)} this run covers. ${status.headline}`
        }
      >
        of {durationNs(endNs)} · {drivingReplay ? "recording" : status.chip}
      </span>
      {syncB ? (
        <span className="chip" data-testid="sync-chip" title="A scrub moves both runs; see the Compare panel">
          B synced{compareSync.offsetNs === 0 ? "" : ` ${(compareSync.offsetNs / 1e9).toFixed(1)} s`}
        </span>
      ) : null}

      {timeline.length > 0 ? (
        <button
          type="button"
          className="icon"
          title={caps.seek.enabled ? "Jump to the next marked event" : caps.seek.why}
          disabled={!caps.seek.enabled}
          onClick={() => {
            const next = timeline.map((m) => m.tNs).filter((t) => t > nowNs).sort((a, b) => a - b)[0];
            if (next !== undefined) seekTo(next);
            else setNotice("No marked event after this point.");
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
              <div className="sec">Latest events — the time jumps there, the description explains it</div>
              {[...timeline]
                .slice(-25)
                .reverse()
                .map((m, i) => (
                  <div className="row" key={`${m.tNs}-${m.channel}-${m.nodeId}-${i}`}>
                    <button
                      type="button"
                      className="linklike"
                      disabled={!caps.seek.enabled}
                      onClick={() => seekTo(m.tNs)}
                      aria-label={`Go to ${simClock(m.tNs)} — ${m.channel}, ${m.label}`}
                    >
                      {simClock(m.tNs)}
                    </button>
                    <span
                      className="dim"
                      style={{ color: MARK_COLOR[m.channel] ?? "var(--accent)" }}
                      title={m.channel}
                    >
                      {CHANNEL_LABEL[m.channel] ?? m.channel}
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

      {notice ? (
        <div className="timebar-notice" role="status" data-testid="time-notice">
          {notice}
          <button type="button" className="linklike" onClick={() => setNotice(null)} aria-label="Dismiss">
            ✕
          </button>
        </div>
      ) : null}
    </div>
  );
}
