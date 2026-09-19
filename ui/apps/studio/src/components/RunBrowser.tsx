/**
 * The run browser of 09-ui §6: "list of recordings and live runs with manifest summaries; open,
 * compare, export".
 *
 * What is live here is what VWP v1 actually exposes: the connected run's `run.status` (§6.6), the
 * `Hello` manifest fields (§3.1.1) and the saved scenarios and runs of `scenario.list` (§6.10).
 * There is no "list recordings" method in the §6.15 inventory, so opening a recording means pointing
 * the replay reader at a file and reconnecting (§7); that part is stubbed and labelled.
 */

import { useCallback, useState } from "react";

import { engine } from "../state/engine.js";
import { useStudio } from "../state/store.js";
import { bytes, durationNs, simClock } from "../lib/format.js";

export function RunBrowser(): React.JSX.Element {
  const hello = useStudio((s) => s.hello);
  const run = useStudio((s) => s.run);
  const world = useStudio((s) => s.world);
  const frames = useStudio((s) => s.frames);
  const list = useStudio((s) => s.scenarioList);
  const [message, setMessage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

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

      {world ? (
        <div className="section">
          <h3>World</h3>
          <dl className="kv">
            <dt>payload</dt>
            <dd>{bytes(world.bytes)}</dd>
            <dt>lanes</dt>
            <dd>{world.lanes}</dd>
            <dt>buildings</dt>
            <dd>{world.buildings}</dd>
            <dt>junctions</dt>
            <dd>{world.junctions}</dd>
            <dt>signals</dt>
            <dd>{world.signals}</dd>
            <dt>sites</dt>
            <dd>{world.sites}</dd>
            <dt>build</dt>
            <dd>
              {world.buildMs.toFixed(0)} ms · {world.buildingBackend} · {world.drawables} drawables
            </dd>
          </dl>
        </div>
      ) : null}

      <div className="section">
        <h3>Frames received</h3>
        <dl className="kv">
          <dt>Keyframe</dt>
          <dd>{frames.keyframe}</dd>
          <dt>Delta</dt>
          <dd>{frames.delta}</dd>
          <dt>Telemetry</dt>
          <dd>{frames.telemetry}</dd>
          <dt>Event</dt>
          <dd>{frames.event}</dd>
          <dt>MetricSample</dt>
          <dd>{frames.metric}</dd>
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
        <h3>Recordings</h3>
        <div className="note">
          <strong>Stub.</strong> §6.15 has no method that lists recordings; a recording is opened by pointing
          the replay reader at an MCAP file (<code>v2xw serve --replay file.mcap</code>, 09-ui §7) and
          reconnecting, which produces a byte-identical stream (§7.2).
        </div>
        <button type="button" disabled={busy} onClick={() => void exportRecording()}>
          export.recording
        </button>
        {message ? <div className="note info">{message}</div> : null}
      </div>
    </div>
  );
}
