/**
 * The run browser of 09-ui §6: "list of recordings and live runs with manifest summaries; open,
 * compare, export".
 *
 * Three sources of truth sit side by side here:
 *
 *  * the connected run's `run.status` (§6.6), the `Hello` manifest fields (§3.1.1) and the saved
 *    scenarios of `scenario.list` (§6.10);
 *  * the decoded world (§4), whose every figure opens its own provenance;
 *  * a **local recording**, opened in this page by `crates/v2xw-wasm` with no engine at all. §6.15
 *    has no method that lists recordings, so "open a recording" is a file, not a query — and 09-ui
 *    §7 makes that the reviewer's path: read somebody else's result without trusting, or even
 *    running, their engine.
 */

import { useCallback, useRef, useState } from "react";

import { engine } from "../state/engine.js";
import { useStudio } from "../state/store.js";
import { bytes, durationNs, int, simClock } from "../lib/format.js";
import { channelSubject, clientSubject, worldSubject } from "../lib/provenance.js";

/** One world figure, as a control that opens what counted it (§4). */
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

/** One frame counter, as a control that opens what counted it (§2.4). */
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

export function RunBrowser(): React.JSX.Element {
  const hello = useStudio((s) => s.hello);
  const run = useStudio((s) => s.run);
  const world = useStudio((s) => s.world);
  const frames = useStudio((s) => s.frames);
  const list = useStudio((s) => s.scenarioList);
  const replay = useStudio((s) => s.replay);
  const setWhy = useStudio((s) => s.setWhy);
  const [message, setMessage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const recordingRef = useRef<HTMLInputElement | null>(null);
  const worldRef = useRef<HTMLInputElement | null>(null);

  const exportRecording = useCallback(async () => {
    setBusy(true);
    setMessage(null);
    try {
      const res = await engine.request("export.recording", { path: `runs/${run.runId || "run"}.mcap` });
      setMessage(`export.recording → ${JSON.stringify(res)}`);
    } catch (err) {
      setMessage(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }, [run.runId]);

  const openRecording = useCallback(async (file: File) => {
    setBusy(true);
    setMessage(null);
    try {
      await engine.openLocalRecording(file);
      setMessage(`opened ${file.name} — scrub it with the time bar; no engine is involved`);
    } catch (err) {
      setMessage(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }, []);

  const openWorld = useCallback(async (file: File) => {
    setBusy(true);
    setMessage(null);
    const ok = await engine.openLocalWorld(file);
    setMessage(
      ok
        ? `adopted ${file.name}; its digest is in the log — a recording carries no Hello.world_hash to check it against (§7.1)`
        : `${file.name} is not a vwp-world/1 payload`,
    );
    setBusy(false);
  }, []);

  return (
    <div className="panel-body" data-testid="run-browser">
      <div className="section">
        <h3>Live run</h3>
        {hello ? (
          <dl className="kv">
            <dt>run id</dt>
            <dd style={{ fontSize: 10 }}>{run.runId || hello.runId}</dd>
            <dt>label</dt>
            <dd>{hello.runLabel || "—"}</dd>
            <dt>state</dt>
            <dd>{run.state}</dd>
            <dt>t</dt>
            <dd>
              {simClock(run.tNs)} / {simClock(run.tEndNs || hello.simDurationNs)}
            </dd>
            <dt>speed</dt>
            <dd>{run.speed}×</dd>
            <dt>profile</dt>
            <dd>
              {run.profile}
              {(hello.flags & 0x2) !== 0 ? " · replay" : " · live"}
              {(hello.flags & 0x40) !== 0 ? " · seekable" : ""}
              {(hello.flags & 0x80) !== 0 ? " · writable" : " · read-only"}
            </dd>
            <dt>actors / nodes</dt>
            <dd>
              {run.actors} / {run.nodes}
            </dd>
            <dt>Δt_mob</dt>
            <dd>{durationNs(hello.mobilityStepNs)}</dd>
            <dt>keyframe</dt>
            <dd>{durationNs(hello.keyframePeriodNs)}</dd>
            <dt>scenario hash</dt>
            <dd style={{ fontSize: 10 }}>{hello.scenarioHash}</dd>
            <dt>world hash</dt>
            <dd style={{ fontSize: 10 }}>{hello.worldHash}</dd>
          </dl>
        ) : (
          <p className="dim">Not connected.</p>
        )}
      </div>

      <div className="section">
        <h3>Recording, opened here</h3>
        <div className="note">
          Read in the page by <code>crates/v2xw-wasm</code> — the same Rust reader the engine uses, compiled
          to WebAssembly (09-ui §7), so the stream it resolves is byte-identical to the live one (§7.2). The
          file is never uploaded and no engine runs.
        </div>
        <div className="row">
          <input
            ref={recordingRef}
            type="file"
            accept=".mcap"
            disabled={busy}
            onChange={(e) => {
              const file = e.target.files?.[0];
              if (file) void openRecording(file);
            }}
            aria-label="Recording file"
            data-testid="open-recording"
          />
        </div>
        {replay !== null ? (
          <>
            <dl className="kv">
              <dt>file</dt>
              <dd>{replay.label}</dd>
              <dt>span</dt>
              <dd>
                {simClock(replay.startNs)} … {simClock(replay.endNs)}
              </dd>
              <dt>at</dt>
              <dd>{simClock(replay.tNs)}</dd>
              <dt>last seek</dt>
              <dd>
                <button
                  type="button"
                  className="linklike mono"
                  data-testid="replay-seek-why"
                  aria-label={`Replay position ${simClock(replay.tNs)} — explain`}
                  onClick={() => setWhy(clientSubject("replay_position", "replay position", simClock(replay.tNs), "simulated time"))}
                >
                  {replay.chunksRead} chunk(s), {replay.requests} range request(s)
                </button>
              </dd>
            </dl>
            {replay.worldUnverified ? (
              <div className="note warn-note" data-testid="world-unverified">
                The geometry on screen came from a file, and a recording carries no{" "}
                <code>Hello.world_hash</code> (§7.1) — so the §10.5 W3 check that normally proves the world
                matches the run had nothing to compare against. Its digest is in the log; check it against the
                run manifest.
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
                aria-label="World payload file"
                data-testid="open-world"
              />
              <button type="button" onClick={() => engine.closeLocalReplay()} data-testid="close-recording">
                close
              </button>
            </div>
            <p className="help">
              A <code>.vwb</code> beside the recording draws its streets. Without one the actors are shown on
              an empty ground plane, which is honest — the recording does not say what they were driving on.
            </p>
          </>
        ) : null}
      </div>

      {world ? (
        <div className="section">
          <h3>World</h3>
          <dl className="kv">
            <dt>payload</dt>
            <dd>
              <WorldFigure field="bytes" text={bytes(world.bytes)} />
            </dd>
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
            <dt>sites</dt>
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
            <dt>build</dt>
            <dd>
              <WorldFigure field="buildMs" text={`${world.buildMs.toFixed(0)} ms`} /> · {world.buildingBackend} ·{" "}
              <WorldFigure field="drawables" text={int(world.drawables)} /> drawables
            </dd>
          </dl>
        </div>
      ) : null}

      <div className="section">
        <h3>Frames received</h3>
        <dl className="kv">
          <dt>Keyframe</dt>
          <dd>
            <FrameCounter channel="snapshot.keyframe" count={frames.keyframe} />
          </dd>
          <dt>Delta</dt>
          <dd>
            <FrameCounter channel="snapshot.delta" count={frames.delta} />
          </dd>
          <dt>Telemetry</dt>
          <dd>
            <FrameCounter channel="node.telemetry" count={frames.telemetry} />
          </dd>
          <dt>Event</dt>
          <dd>
            <FrameCounter channel="events" count={frames.event} />
          </dd>
          <dt>MetricSample</dt>
          <dd>
            <FrameCounter channel="metric.sample" count={frames.metric} />
          </dd>
        </dl>
      </div>

      {list.length > 0 ? (
        <div className="section">
          <h3>Saved scenarios and runs</h3>
          <table className="table">
            <tbody>
              {list.map((item) => (
                <tr key={item.id}>
                  <td title={item.description}>{item.name ?? item.id}</td>
                  <td>{item.kind}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : null}

      <div className="section">
        <h3>Export</h3>
        <button type="button" disabled={busy} onClick={() => void exportRecording()}>
          export.recording
        </button>
        {message ? <div className="note info">{message}</div> : null}
      </div>
    </div>
  );
}
