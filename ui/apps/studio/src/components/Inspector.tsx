/**
 * The right-hand inspector of 09-ui §6: `state · why · log`, with the neighbour table, the
 * certificate store, the CRL and the verify queue underneath, and the docked OBU HUD when the
 * viewport HUD is floated away.
 *
 * The "state" tab is the §3.5.2 record in full — the same numbers the HUD condenses into six lines —
 * plus whatever `inspect.node` (§6.8) adds on top of the binary stream (neighbours, stores, certs).
 */

import { Fragment } from "react";

import { ObuHud } from "./ObuHud.js";
import { WhyTab } from "./WhyTab.js";
import { engine } from "../state/engine.js";
import { useStudio } from "../state/store.js";
import { NA, int, simClock } from "../lib/format.js";
import { hudGroups, queueRows } from "../lib/telemetry.js";

function StateTab(): React.JSX.Element {
  const telemetry = useStudio((s) => s.telemetry);
  const telemetryNode = useStudio((s) => s.telemetryNode);
  const inspect = useStudio((s) => s.inspect);
  const hello = useStudio((s) => s.hello);
  const setWhy = useStudio((s) => s.setWhy);
  const info = telemetryNode !== null ? engine.nodes.get(telemetryNode) : undefined;

  if (telemetryNode === null) {
    return (
      <div className="panel-body">
        <p className="dim">Nothing followed. Click an actor or an RSU in the viewport.</p>
        {hello ? (
          <div className="section">
            <h3>Connection</h3>
            <dl className="kv">
              <dt>run</dt>
              <dd>{hello.runId}</dd>
              <dt>engine</dt>
              <dd>{hello.engineVersion}</dd>
              <dt>scenario</dt>
              <dd>{hello.scenarioName}</dd>
              <dt>world hash</dt>
              <dd style={{ fontSize: 10 }}>{hello.worldHash}</dd>
              <dt>nodes</dt>
              <dd>{hello.nodeCount}</dd>
              <dt>classes</dt>
              <dd>{hello.classNames.join(", ")}</dd>
              <dt>origin</dt>
              <dd>
                {hello.origin.lat.toFixed(5)}, {hello.origin.lon.toFixed(5)}
              </dd>
            </dl>
          </div>
        ) : null}
      </div>
    );
  }

  const groups = telemetry ? hudGroups(telemetry) : [];
  const queues = telemetry ? queueRows(telemetry) : [];
  const neighbors = inspect?.neighbors ?? [];

  return (
    <div className="panel-body" data-testid="inspector-state">
      <div className="section">
        <h3>{info?.label || `node ${telemetryNode}`}</h3>
        <dl className="kv">
          <dt>node id</dt>
          <dd>{telemetryNode}</dd>
          <dt>actor id</dt>
          <dd>{info?.actorId ?? NA}</dd>
          <dt>kind</dt>
          <dd>{inspect?.kind ?? (info?.kind === 2 ? "rsu" : "obu")}</dd>
          <dt>profile</dt>
          <dd>{inspect?.profile_id ?? info?.profileId ?? NA}</dd>
          <dt>sampled at</dt>
          <dd>{inspect ? simClock(inspect.t_ns) : NA}</dd>
        </dl>
      </div>

      {queues.length > 0 ? (
        <div className="section">
          <h3>Queues (§3.5.2)</h3>
          <table className="table">
            <thead>
              <tr>
                <th>queue</th>
                <th>p50</th>
                <th>p95</th>
                <th>drops</th>
              </tr>
            </thead>
            <tbody>
              {queues.map((q) => (
                <tr key={q.id}>
                  <td>
                    {q.label} <span className="faint">{q.unit}</span>
                  </td>
                  <td>{q.p50 === null ? NA : int(q.p50)}</td>
                  <td>{q.p95 === null ? NA : int(q.p95)}</td>
                  <td>
                    {q.drops.length === 0
                      ? "—"
                      : q.drops.map((d) => `${d.label} ${d.value === null ? NA : int(d.value)}`).join(", ")}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : null}

      {neighbors.length > 0 ? (
        <div className="section">
          <h3>Neighbour table ({neighbors.length})</h3>
          <table className="table" data-testid="neighbour-table">
            <thead>
              <tr>
                <th>digest</th>
                <th>state</th>
                <th>msgs</th>
                <th>last seen</th>
              </tr>
            </thead>
            <tbody>
              {neighbors.slice(0, 20).map((n) => (
                <tr key={n.digest}>
                  <td>{n.digest}</td>
                  <td>{n.verify_state}</td>
                  <td>{n.messages ?? "—"}</td>
                  <td>{simClock(n.last_seen_ns)}</td>
                </tr>
              ))}
            </tbody>
          </table>
          {neighbors.length > 20 ? <p className="faint">{neighbors.length - 20} more…</p> : null}
        </div>
      ) : null}

      {groups.map((g) => (
        <div className="section" key={g.id}>
          <h3>{g.title}</h3>
          <dl className="kv">
            {g.fields.map((f) => (
              <Fragment key={f.key}>
                <dt>
                  <button
                    type="button"
                    className="linklike"
                    data-testid={`state-field-${f.key}`}
                    aria-label={`${f.label}: ${f.value} — explain`}
                    aria-describedby={`state-help-${f.key}`}
                    onClick={() =>
                      setWhy({
                        kind: "node_field",
                        id: f.key,
                        label: f.label,
                        node: telemetryNode,
                        value: f.value,
                        unit: f.unit,
                      })
                    }
                  >
                    {f.label}
                    {f.visibility === "GT" ? <span className="gt-tag"> GT</span> : null}
                    <span className="sr-only" id={`state-help-${f.key}`}>
                      {f.key} · {f.unit}
                      {f.help ? ` — ${f.help}` : ""}
                    </span>
                  </button>
                </dt>
                <dd className={f.value.includes(NA) ? "hud-na" : undefined}>{f.value}</dd>
              </Fragment>
            ))}
          </dl>
        </div>
      ))}

      {inspect?.stores ? (
        <div className="section">
          <h3>Stores (inspect.node)</h3>
          <pre className="mono" style={{ fontSize: 10, whiteSpace: "pre-wrap", margin: 0 }}>
            {JSON.stringify(inspect.stores, null, 1)}
          </pre>
        </div>
      ) : (
        <div className="note">
          <code>inspect.node</code> returned no <code>stores</code> section, so the evidence buffer and the
          trust store are unknown for this node (§6.8 lists them as optional).
        </div>
      )}
    </div>
  );
}

function LogTab(): React.JSX.Element {
  const logs = useStudio((s) => s.logs);
  return (
    <div className="panel-body log" data-testid="inspector-log">
      {logs.length === 0 ? <p className="dim">No events yet.</p> : null}
      {logs.map((l, i) => (
        <div className="line" key={`${l.at}-${i}`}>
          <span className={`lvl ${l.level}`}>{l.level}</span>
          <span className="tgt">{l.target}</span>
          <span className="grow">{l.message}</span>
        </div>
      ))}
    </div>
  );
}

export function Inspector(): React.JSX.Element {
  const tab = useStudio((s) => s.inspectorTab);
  const setTab = useStudio((s) => s.setInspectorTab);
  const selectedNode = useStudio((s) => s.selectedNode);
  const hudDocked = useStudio((s) => s.hudDocked);
  const label = selectedNode !== null ? engine.nodes.get(selectedNode)?.label ?? `node ${selectedNode}` : "Inspector";

  return (
    <>
      <div className="panel-head tabs">
        <span style={{ marginRight: 8, color: "var(--text)" }}>{label}</span>
        <button type="button" className={tab === "state" ? "active" : ""} onClick={() => setTab("state")}>
          state
        </button>
        <button
          type="button"
          className={tab === "why" ? "active" : ""}
          onClick={() => setTab("why")}
          data-testid="tab-why"
        >
          why
        </button>
        <button type="button" className={tab === "log" ? "active" : ""} onClick={() => setTab("log")}>
          log
        </button>
      </div>
      {tab === "state" ? <StateTab /> : null}
      {tab === "why" ? <WhyTab /> : null}
      {tab === "log" ? <LogTab /> : null}
      {hudDocked ? (
        <div className="panel-foot" style={{ display: "block", padding: 0 }}>
          <ObuHud docked />
        </div>
      ) : null}
    </>
  );
}
