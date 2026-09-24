/**
 * The right-hand inspector: what one radio is doing, why a number is what it is, and the log.
 *
 * The "state" tab is the followed node's telemetry record in full — the same numbers the HUD
 * condenses into six lines — plus the neighbour table, certificate store and queues the engine adds
 * when asked about that node directly.
 *
 * Its empty state used to read "Nothing followed. Click an actor or an RSU in the viewport", which
 * was an instruction to click something that, on a run with no vehicles on screen, was not there;
 * and underneath it printed a 64-character world digest wrapped across two lines. It now says
 * whether there is anything to click, and the identity of the run lives in the header's Details
 * panel where one can copy it.
 */

import { Fragment } from "react";

import { MessageLog } from "./MessageLog.js";
import { ObuHud } from "./ObuHud.js";
import { WhyTab } from "./WhyTab.js";
import { engine } from "../state/engine.js";
import { useStudio } from "../state/store.js";
import { NA, int, radioCount, simClock } from "../lib/format.js";
import { hudGroups, queueRows } from "../lib/telemetry.js";

function StateTab(): React.JSX.Element {
  const telemetry = useStudio((s) => s.telemetry);
  const telemetryNode = useStudio((s) => s.telemetryNode);
  const inspect = useStudio((s) => s.inspect);
  const hello = useStudio((s) => s.hello);
  const setWhy = useStudio((s) => s.setWhy);
  const actors = useStudio((s) => s.run.actors);
  const nodes = useStudio((s) => s.run.nodes);
  const runState = useStudio((s) => s.run.state);
  const devDetails = useStudio((s) => s.devDetails);
  const info = telemetryNode !== null ? engine.nodes.get(telemetryNode) : undefined;

  if (telemetryNode === null) {
    return (
      <div className="panel-body" data-testid="inspector-empty">
        <p className="dim" data-testid="inspector-empty-message">
          {!hello
            ? "Nothing to inspect yet — no run has been loaded."
            : actors === 1
              ? "Select the vehicle in the viewport and everything it knows appears here: what it is receiving, which certificates it holds, who its neighbours are."
              : actors > 1
                ? `Select any of the ${actors} vehicles or roadside units in the viewport and everything it knows appears here: what it is receiving, which certificates it holds, who its neighbours are.`
                : "There is nothing on the map to select yet. Once the run has vehicles in it, choose one and everything it knows appears here."}
        </p>
        {hello ? (
          <div className="section">
            <h3>This run</h3>
            <dl className="kv">
              <dt>scenario</dt>
              <dd>{hello.scenarioName}</dd>
              <dt>radios</dt>
              <dd data-testid="inspector-radios">{radioCount(nodes, hello.nodeCount, runState)}</dd>
              <dt>vehicle types</dt>
              <dd>{hello.classNames.join(", ") || "—"}</dd>
              <dt>map centre</dt>
              <dd>
                {hello.origin.lat.toFixed(5)}, {hello.origin.lon.toFixed(5)}
              </dd>
            </dl>
            <p className="help">
              The run&rsquo;s identity — its id and the digests of the world and the settings it was computed
              from — is under <b>Details</b> in the header, where each one copies in full.
            </p>
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
        <div className="section" data-testid="inspector-queues">
          <h3>Queues</h3>
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

      <MessageLog />

      {neighbors.length > 0 ? (
        <div className="section">
          <h3>Neighbour table ({neighbors.length})</h3>
          <table className="table" data-testid="neighbour-table">
            <thead>
              <tr>
                <th>neighbour</th>
                <th>state</th>
                <th>msgs / RSSI</th>
                <th>last seen</th>
              </tr>
            </thead>
            <tbody>
              {neighbors.slice(0, 20).map((n, i) => (
                // The live engine answers from its link history (node, heard/lost, RSSI); the
                // spec's row names a certificate digest and a verification state. Show either.
                <tr key={n.digest ?? `node-${n.node ?? i}`}>
                  <td>{n.digest ?? (n.node !== undefined ? `node ${n.node}` : NA)}</td>
                  <td>{n.verify_state ?? n.state ?? NA}</td>
                  <td>
                    {n.messages !== undefined
                      ? n.messages
                      : typeof n.rssi_dbm === "number"
                        ? `${n.rssi_dbm.toFixed(1)} dBm`
                        : "—"}
                  </td>
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
          <h3>Stored on this radio</h3>
          <pre className="mono" style={{ fontSize: 10, whiteSpace: "pre-wrap", margin: 0 }}>
            {JSON.stringify(inspect.stores, null, 1)}
          </pre>
        </div>
      ) : (
        <div className="note">
          This engine does not report what this radio has stored — its evidence buffer and its trust store
          are not modelled at this level of detail, so they are unknown rather than empty.
          {devDetails ? (
            <span className="faint">
              {" "}
              <code>inspect.node</code> returned no <code>stores</code> section, which the interface
              specification allows.
            </span>
          ) : null}
        </div>
      )}
    </div>
  );
}

function LogTab(): React.JSX.Element {
  const logs = useStudio((s) => s.logs);
  return (
    <div className="panel-body log" data-testid="inspector-log">
      {logs.length === 0 ? (
        <p className="dim">
          Nothing to report. Anything the engine or this page has to say about a run — a refused frame, a
          failed command, a world that did not match — appears here as it happens.
        </p>
      ) : null}
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
        <button
          type="button"
          className={tab === "state" ? "active" : ""}
          onClick={() => setTab("state")}
          title="Everything the selected radio is doing right now"
        >
          state
        </button>
        <button
          type="button"
          className={tab === "why" ? "active" : ""}
          onClick={() => setTab("why")}
          data-testid="tab-why"
          title="Where the last number you clicked came from"
        >
          why
        </button>
        <button
          type="button"
          className={tab === "log" ? "active" : ""}
          onClick={() => setTab("log")}
          title="What the engine and this page have reported during this session"
        >
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
