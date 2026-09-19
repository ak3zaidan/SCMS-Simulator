/**
 * The Studio shell — the wireframe of 09-ui §6: scenario/config on the left, the viewport in the
 * centre with the time controls under it, the inspector on the right, and the plots strip across
 * the bottom.
 */

import { useCallback, useEffect, useState } from "react";

import { ComparisonView } from "./components/ComparisonView.js";
import { CopilotPanel } from "./components/CopilotPanel.js";
import { Inspector } from "./components/Inspector.js";
import { PlotsStrip } from "./components/PlotsStrip.js";
import { RunBrowser } from "./components/RunBrowser.js";
import { ScenarioPanel } from "./components/ScenarioPanel.js";
import { StatsReadout } from "./components/StatsReadout.js";
import { TimeControls } from "./components/TimeControls.js";
import { Viewport } from "./components/Viewport.js";
import { applyThemeToDocument } from "./lib/theme.js";
import { engine } from "./state/engine.js";
import { useStudio } from "./state/store.js";

type LeftTab = "scenario" | "runs" | "compare" | "copilot";

const CONNECTION_CLASS: Record<string, string> = {
  streaming: "ok",
  handshaking: "warn",
  connecting: "warn",
  reconnecting: "warn",
  failed: "err",
  closed: "err",
  idle: "",
};

export function App(): React.JSX.Element {
  const [tab, setTab] = useState<LeftTab>("scenario");
  const connection = useStudio((s) => s.connection);
  const hello = useStudio((s) => s.hello);
  const run = useStudio((s) => s.run);
  const theme = useStudio((s) => s.theme);
  const setTheme = useStudio((s) => s.setTheme);
  const [connectError, setConnectError] = useState<string | null>(null);

  // One connection attempt on mount; StrictMode's double-invoke is absorbed by the guard in
  // `engine.connect()`, which tears any previous client down first.
  useEffect(() => {
    let cancelled = false;
    void engine
      .connect()
      .then(() => {
        if (!cancelled) {
          setConnectError(null);
          engine.attachViewer();
        }
      })
      .catch((err: unknown) => {
        if (!cancelled) setConnectError(err instanceof Error ? err.message : String(err));
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const toggleTheme = useCallback(() => {
    const next = theme === "dark" ? "light" : "dark";
    setTheme(next);
    applyThemeToDocument(next);
    engine.viewer?.setTheme(next === "light" ? "light" : "dark");
  }, [theme, setTheme]);

  const reconnect = useCallback(() => {
    setConnectError(null);
    void engine
      .connect()
      .then(() => engine.attachViewer())
      .catch((err: unknown) => setConnectError(err instanceof Error ? err.message : String(err)));
  }, []);

  return (
    <div className="app">
      <header className="topbar">
        <h1>V2X World Simulator · Studio</h1>
        <span className={`pill ${CONNECTION_CLASS[connection] ?? ""}`} data-testid="connection-state">
          {connection}
        </span>
        {hello ? (
          <span className="meta" data-testid="engine-version">
            {hello.engineVersion} · VWP {hello.versionMajor}.{hello.versionMinor} · {hello.scenarioName}
          </span>
        ) : (
          <span className="meta">no Hello yet</span>
        )}
        <span className="spacer" />
        <StatsReadout />
        <span className="meta">
          run {run.runId ? `${run.runId.slice(0, 8)}…` : "—"} · {run.actors} actors
        </span>
        <button type="button" onClick={toggleTheme} data-testid="theme-toggle">
          {theme === "dark" ? "Light" : "Dark"}
        </button>
        {connection === "streaming" ? null : (
          <button type="button" className="primary" onClick={reconnect}>
            Connect
          </button>
        )}
      </header>

      <div className="body">
        <aside className="panel left">
          <div className="panel-head tabs">
            <button type="button" className={tab === "scenario" ? "active" : ""} onClick={() => setTab("scenario")}>
              Scenario
            </button>
            <button type="button" className={tab === "runs" ? "active" : ""} onClick={() => setTab("runs")}>
              Runs
            </button>
            <button type="button" className={tab === "compare" ? "active" : ""} onClick={() => setTab("compare")}>
              Compare
            </button>
            <button type="button" className={tab === "copilot" ? "active" : ""} onClick={() => setTab("copilot")}>
              Copilot
            </button>
          </div>
          {tab === "scenario" ? <ScenarioPanel /> : null}
          {tab === "runs" ? <RunBrowser /> : null}
          {tab === "compare" ? <ComparisonView /> : null}
          {tab === "copilot" ? <CopilotPanel /> : null}
        </aside>

        <main className="centre">
          <Viewport />
          <TimeControls />
        </main>

        <aside className="panel right">
          <Inspector />
        </aside>
      </div>

      <PlotsStrip />

      {connectError ? (
        <div className="note err" style={{ position: "fixed", bottom: 12, right: 12, maxWidth: 420, zIndex: 100 }}>
          Could not reach the engine: {connectError}. Start the mock with{" "}
          <code>node packages/mock-server/dist/index.js --actors 200 --port 8787</code>.
        </div>
      ) : null}
    </div>
  );
}
