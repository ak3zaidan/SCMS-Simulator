/**
 * The run's state, said in words, with the button that resolves it.
 *
 * Three pieces, all reading one description from `lib/status.ts`:
 *
 *  * {@link StatusPill} — the chip in the header. Sentence case, never a wire token.
 *  * {@link StatusBanner} — the sentence over the viewport, shown only when the state is not "a run
 *    playing normally". This is the part that was missing: a finished run used to present the word
 *    "closed" and a Connect button that appeared dead, with no explanation anywhere on the page.
 *  * {@link useStatus} — the description itself, for anything else that needs it.
 *
 * The chip keeps the wire tokens in `data-state` and `data-run-state` rather than in its text, so
 * the protocol state is still one query away for a test or a debugger without being the headline a
 * researcher reads.
 */

import { useCallback, useMemo, useState } from "react";

import { engine } from "../state/engine.js";
import { useStudio } from "../state/store.js";
import { durationNs, simClock } from "../lib/format.js";
import { describeStatus, toneClass, type StatusActionKind, type StatusView } from "../lib/status.js";

/** The description of whatever the app is doing right now. */
export function useStatus(): StatusView {
  const connection = useStudio((s) => s.connection);
  const runState = useStudio((s) => s.run.state);
  const tEndNs = useStudio((s) => s.run.tEndNs);
  const tNs = useStudio((s) => s.run.tNs);
  const hello = useStudio((s) => s.hello);
  const replay = useStudio((s) => s.replay);
  const target = useStudio((s) => s.target);
  const reconnectAttempts = useStudio((s) => s.reconnectAttempts);

  return useMemo(() => {
    const span = tEndNs > 0 ? tEndNs : hello?.simDurationNs ?? 0;
    return describeStatus({
      connection,
      runState,
      replayOpen: replay !== null,
      ...(replay ? { replayLabel: replay.label } : {}),
      hasHello: hello !== null,
      ...(hello?.scenarioName ? { scenarioName: hello.scenarioName } : {}),
      targetLabel: target.baseUrl === "" ? "this page's own address" : target.baseUrl,
      targetReachable: target.reachable,
      ...(span > 0 ? { spanText: durationNs(span) } : {}),
      clockText: simClock(tNs),
      reconnectAttempts,
    });
  }, [connection, runState, tEndNs, tNs, hello, replay, target, reconnectAttempts]);
}

/**
 * Perform the action a status description offers.
 *
 * `run` goes through `engine.startRun`, which picks a transport: the socket when one is streaming,
 * plain HTTP when it is not. That is what makes "Run again" work on a page whose stream the engine
 * closed when the run ended — the case that started all of this.
 */
export function useStatusAction(onConnect: () => void): {
  perform: (kind: StatusActionKind) => void;
  busy: boolean;
  error: string | null;
} {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const perform = useCallback(
    (kind: StatusActionKind) => {
      if (kind === "connect") {
        setError(null);
        onConnect();
        return;
      }
      if (kind === "none") return;
      setBusy(true);
      setError(null);
      void (async () => {
        try {
          if (kind === "run") {
            await engine.startRun();
            // A run started from a closed page has nothing streaming to it yet.
            if (!engine.streaming) onConnect();
          } else if (kind === "resume") {
            await engine.request("run.resume", {});
            await engine.refreshStatus();
          } else if (kind === "pause") {
            await engine.request("run.pause", {});
            await engine.refreshStatus();
          }
        } catch (err) {
          setError(err instanceof Error ? err.message : String(err));
        } finally {
          setBusy(false);
        }
      })();
    },
    [onConnect],
  );

  return { perform, busy, error };
}

/**
 * The one button in the header worth pressing next.
 *
 * A first-time user opening this page had nothing to press: four tabs of equal weight, a Connect
 * button that only appeared when the stream was down, and the transport hidden in a bar under the
 * viewport. Whatever state the app is in, this is the next step — Run, Play, Pause, Connect — and
 * it is always in the same place.
 */
export function PrimaryAction({ onConnect }: { onConnect: () => void }): React.JSX.Element | null {
  const status = useStatus();
  const runState = useStudio((s) => s.run.state);
  const replay = useStudio((s) => s.replay);
  const { perform, busy } = useStatusAction(onConnect);

  // A recording has no transport: there is nothing to start, pause or resume, only a position.
  if (replay !== null) return null;

  const action =
    status.action ??
    (runState === "running"
      ? { label: "Pause", kind: "pause" as const, hint: "Hold the simulated clock where it is" }
      : null);
  if (action === null) return null;

  return (
    <button
      type="button"
      className="primary"
      disabled={busy}
      title={action.hint}
      data-testid="primary-action"
      onClick={() => perform(action.kind)}
    >
      {busy ? "working…" : action.label}
    </button>
  );
}

/** The header chip: the state in words, the wire tokens in attributes. */
export function StatusPill(): React.JSX.Element {
  const status = useStatus();
  const connection = useStudio((s) => s.connection);
  const runState = useStudio((s) => s.run.state);
  return (
    <span
      className={`pill ${toneClass(status.tone)}`}
      data-testid="connection-state"
      data-state={connection}
      data-run-state={runState}
      title={`${status.headline}${status.detail ? ` ${status.detail}` : ""}`}
    >
      {status.chip}
    </span>
  );
}

/**
 * The sentence over the viewport, for every state that is not a run in normal motion.
 *
 * Deliberately in the flow above the viewport rather than floating over it: a message that covers
 * the picture is a message people dismiss, and this one is often the only thing on the page worth
 * reading.
 */
export function StatusBanner({
  onConnect,
  note = null,
}: {
  onConnect: () => void;
  /**
   * What the last connection attempt reported, when it failed.
   *
   * Carried here rather than shown in a box of its own: a floating note saying "nothing answered"
   * beside a banner saying "nothing is answering" is two messages for one fact, which is the
   * clutter this whole pass is about. The detail the note adds — the actual failure — belongs under
   * the sentence that already has the reader's attention.
   */
  note?: string | null;
}): React.JSX.Element | null {
  const status = useStatus();
  const { perform, busy, error } = useStatusAction(onConnect);
  if (!status.banner) return null;

  return (
    <div className={`statusbar ${status.tone}`} data-testid="status-banner" role="status">
      <div className="statusbar-text">
        <strong data-testid="status-headline">{status.headline}</strong>
        {status.detail ? <span className="dim"> {status.detail}</span> : null}
        {error ? <span className="statusbar-error"> That did not work: {error}</span> : null}
        {note ? (
          <div className="faint" data-testid="status-note">
            The last attempt reported: {note}
          </div>
        ) : null}
      </div>
      {status.action ? (
        <button
          type="button"
          className="primary"
          disabled={busy}
          title={status.action.hint}
          data-testid="status-action"
          onClick={() => perform(status.action?.kind ?? "none")}
        >
          {busy ? "working…" : status.action.label}
        </button>
      ) : null}
    </div>
  );
}
