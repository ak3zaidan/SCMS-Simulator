/**
 * The copilot panel of 09-ui §6 — "chat that calls the same JSON-RPC methods (tool registry
 * generated from the method list), showing each call and result".
 *
 * No LLM is wired up yet. What is wired up is the part that would otherwise drift: the tool surface
 * is `rpc.discover` (§6.15), fetched from the engine at connect time, not a list typed out here. If
 * the engine gains a method, this panel grows a tool; if this build knows a method the engine does
 * not implement, the row says so. Underneath, every JSON-RPC call the Studio has made is logged, so
 * the "show each call" half of the panel is real today.
 */

import { useMemo, useState } from "react";
import { VWP_METHODS } from "@vwp/protocol";

import { engine } from "../state/engine.js";
import { useStudio } from "../state/store.js";

export function CopilotPanel(): React.JSX.Element {
  const methods = useStudio((s) => s.rpcMethods);
  const calls = useStudio((s) => s.rpcCalls);
  const [filter, setFilter] = useState("");
  const [draft, setDraft] = useState("");

  const known = useMemo(() => new Set(methods.map((m) => m.name)), [methods]);
  const missing = useMemo(() => VWP_METHODS.filter((m) => methods.length > 0 && !known.has(m)), [known, methods.length]);
  const shown = useMemo(
    () => methods.filter((m) => filter === "" || m.name.includes(filter) || m.summary.toLowerCase().includes(filter.toLowerCase())),
    [methods, filter],
  );

  return (
    <>
      <div className="panel-body" data-testid="copilot-panel">
        <div className="note info">
          <strong>No assistant is connected in this build.</strong> What is here is the list of everything
          this engine can be asked to do — {methods.length} commands, published by the engine itself, so the
          list cannot fall out of date — and a record of every command this page has sent.
        </div>

        <div className="field">
          <label htmlFor="copilot-filter">Filter</label>
          <input id="copilot-filter" type="text" value={filter} onChange={(e) => setFilter(e.target.value)} placeholder="run., inspect., overlay…" />
        </div>

        <div className="section">
          <h3>What this engine accepts ({shown.length})</h3>
          <div className="method-list" data-testid="rpc-methods">
            {shown.map((m) => (
              <div className="m" key={m.name}>
                <span className="name">{m.name}</span>
                <span className="sum" title={m.summary}>
                  {m.summary}
                </span>
              </div>
            ))}
            {shown.length === 0 ? (
              <p className="dim">
                {filter === ""
                  ? "This engine did not publish a list of what it accepts. Press refresh below to ask again."
                  : `Nothing matches “${filter}”.`}
              </p>
            ) : null}
          </div>
        </div>

        {missing.length > 0 ? (
          <div className="section">
            <h3>Commands this page knows but this engine does not offer</h3>
            <div className="method-list">
              {missing.map((m) => (
                <div className="m" key={m}>
                  <span className="name faint">{m}</span>
                </div>
              ))}
            </div>
          </div>
        ) : null}

        <div className="section">
          <h3>Commands sent from this page ({calls.length})</h3>
          <div className="method-list">
            {calls.map((c, i) => (
              <div className="m" key={`${c.method}-${c.at}-${i}`}>
                <span className="name">{c.method}</span>
                <span className="sum">{new Date(c.at).toLocaleTimeString()}</span>
              </div>
            ))}
            {calls.length === 0 ? (
              <p className="dim">
                Nothing sent yet. Every button in the interface sends one of the commands above, and each
                one appears here as it goes out.
              </p>
            ) : null}
          </div>
        </div>
      </div>

      <div className="panel-foot">
        <input
          type="text"
          placeholder="ask… (no assistant in this build)"
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          aria-label="Copilot prompt"
          disabled
        />
        <button type="button" disabled title="No assistant is connected in this build">
          send
        </button>
        <button
          type="button"
          title="Ask the engine again for the list of what it accepts"
          onClick={() => void engine.refreshRpcMethods()}
        >
          refresh
        </button>
      </div>
    </>
  );
}
