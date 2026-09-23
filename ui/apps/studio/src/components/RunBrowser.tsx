/**
 * The Runs tab: the run on screen, the recordings opened in this page, and the world under them.
 *
 * # What it is for, which was not clear before
 *
 * The tab used to be a manifest dump — nine rows of `run.status` and `Hello` fields, two
 * 64-character digests printed in full, five frame counters, the world's ten figures, a file picker
 * and an `export.recording` button labelled with the method name — with the live run, the world and
 * a recording all at the same level. Most of it now lives in the header's Details disclosure, where
 * identifiers belong, so what is left can do the job the tab is named for.
 *
 * # Several recordings
 *
 * This is where a researcher with four recordings works, and the tab could not hold four. §6.15 has
 * no method that lists recordings — there is no `runs.list`, and the server would not know about a
 * file on your disk if there were — so "open a recording" is a file, not a query, and each one used
 * to displace the last with nothing remembering that the others existed. Every recording opened
 * here now stays in the list (`store.ts`'s `recordings`), which makes the two things you actually do
 * with several of them one click each: show one in the main viewport, or put one beside the live run
 * in the comparison pane. The list is session-scoped — a browser will not let a page keep a file
 * handle across a reload — and it says so, rather than looking like a library that lost its
 * contents.
 *
 * # What is kept, deliberately
 *
 * Every world figure and every frame counter is still a control that opens what produced it. That
 * property is the most valuable thing this interface has and the cleanup does not get to spend it:
 * the figures are quieter and the counters are behind a disclosure, but both still explain
 * themselves.
 */

import { useCallback, useRef, useState } from "react";

import { Identifier } from "./Identifier.js";
import { useStatus } from "./Status.js";
import { compare } from "../state/compare.js";
import { engine } from "../state/engine.js";
import { useStudio, type RecordingEntry } from "../state/store.js";
import { bytes, durationNs, int, simClock } from "../lib/format.js";
import { channelSubject, clientSubject, worldSubject } from "../lib/provenance.js";

/** One world figure, as a control that opens what counted it. */
function WorldFigure({ field, text }: { field: string; text: string }): React.JSX.Element {
  const setWhy = useStudio((s) => s.setWhy);
  return (
    <button
      type="button"
      className="linklike mono"
      data-testid={`world-${field}`}
      aria-label={`${field}: ${text} — explain`}
      onClick={() => setWhy(worldSubject(field, text))}
    >
      {text}
    </button>
  );
}

/** One frame counter, as a control that opens what counted it. */
function FrameCounter({ channel, count }: { channel: string; count: number }): React.JSX.Element {
  const setWhy = useStudio((s) => s.setWhy);
  return (
    <button
      type="button"
      className="linklike mono"
      data-testid={`frames-${channel}`}
      aria-label={`${channel} frames decoded: ${count} — explain`}
      onClick={() => setWhy(channelSubject(channel, `${int(count)} frames decoded`))}
    >
      {int(count)}
    </button>
  );
}

/** A stable key for a file, so opening the same one twice updates a row rather than adding one. */
function recordingId(file: File): string {
  return `${file.name}:${file.size}`;
}

export function RunBrowser(): React.JSX.Element {
  const hello = useStudio((s) => s.hello);
  const run = useStudio((s) => s.run);
  const world = useStudio((s) => s.world);
  const frames = useStudio((s) => s.frames);
  const replay = useStudio((s) => s.replay);
  const recordings = useStudio((s) => s.recordings);
  const currentRecording = useStudio((s) => s.currentRecording);
  const compareSide = useStudio((s) => s.compare);
  const setWhy = useStudio((s) => s.setWhy);
  const status = useStatus();
  const [message, setMessage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const worldRef = useRef<HTMLInputElement | null>(null);

  const exportRecording = useCallback(async () => {
    setBusy(true);
    setMessage(null);
    try {
      const path = `runs/${run.runId || "run"}.mcap`;
      const res = await engine.request("export.recording", { path });
      // The result is either a finished export or a job; both carry something worth naming, and
      // neither is worth printing as raw JSON at somebody.
      const files = (res as { files?: { path?: string }[] }).files;
      setMessage(
        files && files.length > 0
          ? `Written: ${files.map((f) => f.path ?? "?").join(", ")}.`
          : `The engine accepted the request for ${path}.`,
      );
    } catch (err) {
      // The engine's own sentence here is better than anything this panel could write: it says
      // "this run writes no recording: start the server with --record <path>", which is the fix.
      setMessage(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }, [run.runId]);

  /** Open a recording in the main viewport, remembering it so it can be reopened. */
  const showRecording = useCallback(async (file: File) => {
    setBusy(true);
    setMessage(null);
    const id = recordingId(file);
    try {
      await engine.openLocalRecording(file);
      const store = useStudio.getState();
      const span = store.replay;
      const entry: RecordingEntry = {
        id,
        name: file.name,
        bytes: file.size,
        startNs: span?.startNs ?? 0,
        endNs: span?.endNs ?? 0,
        openedAt: Date.now(),
        file,
      };
      store.noteRecording(entry);
      store.setCurrentRecording(id);
      setMessage(`Showing ${file.name}. Drag the time bar to move through it — no engine is involved.`);
    } catch (err) {
      setMessage(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }, []);

  /** Put a recording beside the live run in the comparison pane. */
  const compareRecording = useCallback(async (entry: RecordingEntry) => {
    setBusy(true);
    setMessage(null);
    try {
      await compare.openRecording(entry.file);
      setMessage(`${entry.name} is open beside this run. The Compare tab has the difference table.`);
    } catch (err) {
      setMessage(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }, []);

  const closeRecording = useCallback(() => {
    engine.closeLocalReplay();
    useStudio.getState().setCurrentRecording(null);
    setMessage("Back to the live run.");
  }, []);

  const openWorld = useCallback(async (file: File) => {
    setBusy(true);
    setMessage(null);
    const ok = await engine.openLocalWorld(file);
    setMessage(
      ok
        ? `Using the streets from ${file.name}. A recording does not say which world it was made on, so nothing here proves these are the right ones — the file's digest is in the log, to check against the run's manifest.`
        : `${file.name} is not a world payload this build can read.`,
    );
    setBusy(false);
  }, []);

  return (
    <div className="panel-body" data-testid="run-browser">
      <div className="section">
        <h3>This run</h3>
        {hello ? (
          <>
            <dl className="kv">
              <dt>scenario</dt>
              <dd>{hello.scenarioName || "—"}</dd>
              <dt>state</dt>
              <dd>{status.chip}</dd>
              <dt>reached</dt>
              <dd>
                {simClock(run.tNs)} of {durationNs(run.tEndNs || hello.simDurationNs)}
              </dd>
              <dt>on the road</dt>
              <dd>
                {int(run.actors)} {run.actors === 1 ? "vehicle" : "vehicles"} · {int(run.nodes)}{" "}
                {run.nodes === 1 ? "radio" : "radios"}
              </dd>
            </dl>
            <details className="disclose">
              <summary>Identity, so this run can be cited</summary>
              <div className="disclose-body">
                <dl className="kv">
                  <dt>run</dt>
                  <dd>
                    <Identifier value={run.runId || hello.runId} label="run id" testId="run-id" />
                  </dd>
                  <dt>label</dt>
                  <dd>{hello.runLabel || <span className="faint">none</span>}</dd>
                  <dt>scenario</dt>
                  <dd>
                    <Identifier value={hello.scenarioHash} label="scenario digest" />
                  </dd>
                  <dt>world</dt>
                  <dd>
                    <Identifier value={hello.worldHash} label="world digest" />
                  </dd>
                  <dt>steps every</dt>
                  <dd>{durationNs(hello.mobilityStepNs)}</dd>
                  <dt>full snapshot every</dt>
                  <dd>{durationNs(hello.keyframePeriodNs)}</dd>
                  <dt>this connection sees</dt>
                  <dd>
                    {run.profile === "node" ? "what a radio would see" : "everything, ground truth included"}
                    {(hello.flags & 0x2) !== 0 ? " · replayed" : " · live"}
                    {(hello.flags & 0x40) !== 0 ? " · seekable" : ""}
                  </dd>
                </dl>
              </div>
            </details>
            <details className="disclose">
              <summary>Frames decoded, with what counted each one</summary>
              <div className="disclose-body">
                <dl className="kv">
                  <dt>full snapshots</dt>
                  <dd>
                    <FrameCounter channel="snapshot.keyframe" count={frames.keyframe} />
                  </dd>
                  <dt>movement updates</dt>
                  <dd>
                    <FrameCounter channel="snapshot.delta" count={frames.delta} />
                  </dd>
                  <dt>radio telemetry</dt>
                  <dd>
                    <FrameCounter channel="node.telemetry" count={frames.telemetry} />
                  </dd>
                  <dt>events</dt>
                  <dd>
                    <FrameCounter channel="events" count={frames.event} />
                  </dd>
                  <dt>measurements</dt>
                  <dd>
                    <FrameCounter channel="metric.sample" count={frames.metric} />
                  </dd>
                </dl>
              </div>
            </details>
          </>
        ) : (
          <p className="dim">{status.headline}</p>
        )}
      </div>

      <div className="section">
        <h3>Recordings ({recordings.length})</h3>
        <p className="help">
          A recording is read here in the page, by the same reader the engine uses, compiled to
          WebAssembly. The file is never uploaded and no engine runs. Recordings opened here stay in this
          list until the page is reloaded — a browser will not let a page keep a file across one.
        </p>
        <div className="row">
          <input
            type="file"
            accept=".mcap"
            disabled={busy}
            onChange={(e) => {
              const file = e.target.files?.[0];
              if (file) void showRecording(file);
              // Cleared so choosing the same file twice fires a change event both times.
              e.target.value = "";
            }}
            aria-label="Open a recording file"
            data-testid="open-recording"
          />
        </div>

        {recordings.length === 0 ? (
          <p className="dim">
            None open. A run writes one when the engine is started with <code>--record &lt;path&gt;</code>; the
            files are <code>.mcap</code>.
          </p>
        ) : (
          <div data-testid="recording-list">
            {recordings.map((r) => {
              const showing = currentRecording === r.id && replay !== null;
              const comparing = compareSide?.label === r.name;
              return (
                <div className={showing ? "runrow current" : "runrow"} key={r.id}>
                  <span className="name">
                    {r.name}
                    {showing ? <span className="dim"> · on screen</span> : null}
                    {comparing ? <span className="dim"> · beside this run</span> : null}
                    <br />
                    <span className="meta">
                      {simClock(r.startNs)}–{simClock(r.endNs)} · {bytes(r.bytes)}
                    </span>
                  </span>
                  {showing ? (
                    <button type="button" onClick={closeRecording} data-testid="close-recording">
                      close
                    </button>
                  ) : (
                    <button
                      type="button"
                      disabled={busy}
                      title="Show this recording in the main viewport instead of the live run"
                      onClick={() => void showRecording(r.file)}
                    >
                      show
                    </button>
                  )}
                  <button
                    type="button"
                    disabled={busy || comparing}
                    title="Open it beside the live run, on one shared clock"
                    onClick={() => void compareRecording(r)}
                  >
                    compare
                  </button>
                  <button
                    type="button"
                    className="linklike"
                    title="Forget this one; the file on disk is untouched"
                    onClick={() => useStudio.getState().forgetRecording(r.id)}
                    aria-label={`Forget ${r.name}`}
                  >
                    ✕
                  </button>
                </div>
              );
            })}
          </div>
        )}

        {replay !== null ? (
          <>
            <dl className="kv">
              <dt>at</dt>
              <dd>{simClock(replay.tNs)}</dd>
              <dt>last move cost</dt>
              <dd>
                <button
                  type="button"
                  className="linklike mono"
                  data-testid="replay-seek-why"
                  aria-label={`Replay position ${simClock(replay.tNs)} — explain`}
                  onClick={() => setWhy(clientSubject("replay_position", "replay position", simClock(replay.tNs), "simulated time"))}
                >
                  {replay.chunksRead} chunk(s), {replay.requests} read(s)
                </button>
              </dd>
            </dl>
            {replay.worldUnverified ? (
              <div className="note warn-note" data-testid="world-unverified">
                The streets on screen came from a file you chose, and a recording carries nothing that says
                which world it was made on — so nothing here proves these are the right streets. The file&rsquo;s
                digest is in the log; check it against the run&rsquo;s manifest.
              </div>
            ) : null}
            <div className="row">
              <input
                ref={worldRef}
                type="file"
                accept=".vwb"
                disabled={busy}
                onChange={(e) => {
                  const file = e.target.files?.[0];
                  if (file) void openWorld(file);
                }}
                aria-label="World file to draw the streets from"
                data-testid="open-world"
              />
            </div>
            <p className="help">
              A <code>.vwb</code> beside the recording draws its streets. Without one the vehicles are shown on
              empty ground, which is honest — the recording does not say what they were driving on.
            </p>
          </>
        ) : null}
      </div>

      {world ? (
        <div className="section">
          <h3>World</h3>
          <p className="help">Every figure here opens the model, the parameters and the source that produced it.</p>
          <dl className="kv">
            <dt>lanes</dt>
            <dd>
              <WorldFigure field="lanes" text={int(world.lanes)} />
            </dd>
            <dt>buildings</dt>
            <dd>
              <WorldFigure field="buildings" text={int(world.buildings)} />
            </dd>
            <dt>junctions</dt>
            <dd>
              <WorldFigure field="junctions" text={int(world.junctions)} />
            </dd>
            <dt>signals</dt>
            <dd>
              <WorldFigure field="signals" text={int(world.signals)} />
            </dd>
            <dt>roadside units</dt>
            <dd>
              <WorldFigure field="sites" text={int(world.sites)} />
            </dd>
            <dt>crossings</dt>
            <dd>
              <WorldFigure field="crossings" text={int(world.crossings)} />
            </dd>
            <dt>land use</dt>
            <dd>
              <WorldFigure field="landuse" text={int(world.landuse)} />
            </dd>
          </dl>
          <details className="disclose">
            <summary>How it was built</summary>
            <div className="disclose-body">
              <dl className="kv">
                <dt>payload</dt>
                <dd>
                  <WorldFigure field="bytes" text={bytes(world.bytes)} />
                </dd>
                <dt>build time</dt>
                <dd>
                  <WorldFigure field="buildMs" text={`${world.buildMs.toFixed(0)} ms`} />
                </dd>
                <dt>buildings by</dt>
                <dd>{world.buildingBackend}</dd>
                <dt>drawables</dt>
                <dd>
                  <WorldFigure field="drawables" text={int(world.drawables)} />
                </dd>
              </dl>
            </div>
          </details>
        </div>
      ) : null}

      <div className="section">
        <h3>Save a recording of this run</h3>
        <p className="help">
          This works only if the engine was started with <code>--record &lt;path&gt;</code>: a run that writes
          no recording has nothing to export, and it will say so.
        </p>
        <button type="button" disabled={busy} onClick={() => void exportRecording()} data-testid="export-recording">
          {busy ? "working…" : "Save recording"}
        </button>
      </div>

      {message ? (
        <div className="note info" data-testid="run-browser-message">
          {message}
        </div>
      ) : null}
    </div>
  );
}
