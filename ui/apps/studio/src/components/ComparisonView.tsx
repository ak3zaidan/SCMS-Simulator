/**
 * The comparison view of 09-ui §6 — a stub, deliberately.
 *
 * "Two to four runs side by side with synchronised time, difference overlays for metrics, and
 * manifest diff." Two of the three pieces need protocol support that exists on paper but not in the
 * fixture: `metrics.query {runs: [...]}` takes a run list (§6.12) and `experiment.compare` is
 * explicitly reserved for a later minor version (§6.15). So this panel shows the shape of the view,
 * fills the first column from the connected run, and states what is missing rather than faking a
 * second column.
 */

import { useCallback, useState } from "react";

import { engine } from "../state/engine.js";
import { useStudio } from "../state/store.js";
import { simClock } from "../lib/format.js";

interface Slot {
  readonly runId: string;
  readonly rows: readonly (readonly [string, string])[];
}

export function ComparisonView(): React.JSX.Element {
  const hello = useStudio((s) => s.hello);
  const run = useStudio((s) => s.run);
  const [other, setOther] = useState("");
  const [slots, setSlots] = useState<readonly Slot[]>([]);
  const [error, setError] = useState<string | null>(null);

  const addRun = useCallback(async () => {
    setError(null);
    try {
      const status = await engine.request("run.status", { run_id: other });
      setSlots((s) => [
        ...s,
        {
          runId: status.run_id,
          rows: [
            ["state", status.state],
            ["t", simClock(status.t_ns)],
            ["actors", String(status.actors ?? 0)],
            ["speed", `${status.speed}×`],
          ],
        },
      ]);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }, [other]);

  const current: Slot | null = hello
    ? {
        runId: run.runId || hello.runId,
        rows: [
          ["state", run.state],
          ["t", simClock(run.tNs)],
          ["actors", String(run.actors)],
          ["speed", `${run.speed}×`],
          ["scenario", hello.scenarioName],
          ["scenario hash", `${hello.scenarioHash.slice(0, 12)}…`],
          ["world hash", `${hello.worldHash.slice(0, 12)}…`],
        ],
      }
    : null;

  const all = current ? [current, ...slots] : slots;

  return (
    <div className="panel-body" data-testid="comparison-view">
      <div className="note">
        <strong>Stub.</strong> Side-by-side comparison needs <code>metrics.query&nbsp;{"{runs:[…]}"}</code>{" "}
        (§6.12) against several runs and <code>experiment.compare</code>, which §6.15 reserves for a later
        minor version. The columns below are manifest summaries only — no difference overlays yet.
      </div>

      <div className="row">
        <input
          type="text"
          placeholder="run id to add"
          value={other}
          onChange={(e) => setOther(e.target.value)}
          aria-label="Run id"
        />
        <button type="button" onClick={() => void addRun()} disabled={other === ""}>
          add
        </button>
      </div>
      {error ? <div className="note err">{error}</div> : null}

      <div className="section" style={{ marginTop: 10 }}>
        <table className="table">
          <thead>
            <tr>
              <th>field</th>
              {all.map((s) => (
                <th key={s.runId}>{s.runId.slice(0, 8)}…</th>
              ))}
            </tr>
          </thead>
          <tbody>
            {(all[0]?.rows ?? []).map(([key], i) => (
              <tr key={key}>
                <td>{key}</td>
                {all.map((s) => (
                  <td key={`${s.runId}-${key}`} className={i > 0 && all.length > 1 && s.rows[i]?.[1] !== all[0].rows[i]?.[1] ? "hud-missing" : undefined}>
                    {s.rows[i]?.[1] ?? "—"}
                  </td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}
