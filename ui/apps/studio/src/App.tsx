/**
 * The Studio shell — the wireframe of 09-ui §6: scenario/config on the left, the viewport in the
 * centre with the time controls under it, the inspector on the right, and the plots strip across
 * the bottom.
 *
 * Two things here are not in that sketch:
 *
 *  * **The engine target.** The Studio no longer assumes the fixture. On mount it resolves a base
 *    URL with `lib/target.ts` — the page's own origin when something answers there, otherwise
 *    `crates/v2xw-server`'s default bind, preferred over the fixture's — and the topbar says which
 *    implementation answered. That readout is the point: a plot of fixture values and a plot of
 *    simulation results must not look the same.
 *  * **The comparison pane.** With side B open the centre splits in two, one `Viewer` each, and
 *    the time controls underneath drive both (09-ui §6, "two runs side by side with synchronised
 *    time").
 */

import { useCallback, useEffect, useState } from "react";

import { ComparePane } from "./components/ComparePane.js";
import { ComparisonView } from "./components/ComparisonView.js";
import { CopilotPanel } from "./components/CopilotPanel.js";
import { EngineTargetChip } from "./components/EngineTarget.js";
import { Inspector } from "./components/Inspector.js";
import { PlotsStrip } from "./components/PlotsStrip.js";
import { RunBrowser } from "./components/RunBrowser.js";
import { ScenarioPanel } from "./components/ScenarioPanel.js";
import { StatsReadout } from "./components/StatsReadout.js";
import { TimeControls } from "./components/TimeControls.js";
import { Viewport } from "./components/Viewport.js";
import { applyThemeToDocument } from "./lib/theme.js";
import {
  candidateTargets,
  readOverride,
  resolveEngineTarget,
  worldFetchBlocked,
} from "./lib/target.js";
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
  const compare = useStudio((s) => s.compare);
  const [connectError, setConnectError] = useState<string | null>(null);

  /**
   * Resolve a target, then connect to it.
   *
   * Kept as one function because the two are one decision: connecting to an engine the app has not
   * identified is how the Studio ended up silently pinned to the fixture. The probe is cheap — one
   * `GET /healthz` per candidate, in sequence, with a short timeout — and its result is what the
   * topbar chip reports.
   */
  const connectResolved = useCallback(async (): Promise<void> => {
    const storage = typeof localStorage === "undefined" ? null : localStorage;
    const override = readOverride(window.location.search, storage);
    const resolved = await resolveEngineTarget(candidateTargets(override), fetch);
    useStudio.getState().setTarget({
      baseUrl: resolved.baseUrl,
      flavour: resolved.probe?.flavour ?? "unknown",
      engine: resolved.probe?.engine ?? "no engine answered",
      reachable: resolved.probe?.reachable ?? false,
      pinned: resolved.pinned,
      tried: resolved.tried,
      worldBlocked: worldFetchBlocked(resolved.baseUrl, window.location.origin),
    });
    await engine.connect(resolved.baseUrl === "" ? window.location.origin : resolved.baseUrl);
    engine.attachViewer();
  }, []);

  // One resolve-and-connect on mount; StrictMode's double-invoke is absorbed by the guard in
  // `engine.connect()`, which tears any previous client down first.
  useEffect(() => {
    let cancelled = false;
    void connectResolved()
      .then(() => {
        if (!cancelled) setConnectError(null);
      })
      .catch((err: unknown) => {
        if (!cancelled) setConnectError(err instanceof Error ? err.message : String(err));
      });
    return () => {
      cancelled = true;
    };
  }, [connectResolved]);

  const toggleTheme = useCallback(() => {
    const next = theme === "dark" ? "light" : "dark";
    setTheme(next);
    applyThemeToDocument(next);
    engine.viewer?.setTheme(next === "light" ? "light" : "dark");
  }, [theme, setTheme]);

  const reconnect = useCallback(() => {
    setConnectError(null);
    void connectResolved().catch((err: unknown) => setConnectError(err instanceof Error ? err.message : String(err)));
  }, [connectResolved]);



  return (
    <div className="app">
      <header className="topbar">
        <h1>V2X World Simulator · Studio</h1>
        <span className={`pill ${CONNECTION_CLASS[connection] ?? ""}`} data-testid="connection-state">
          {connection}
        </span>
        <EngineTargetChip onRetarget={reconnect} />
        {hello ? (
          <span className="meta" data-testid="engine-version">
            {hello.engineVersion} · VWP {hello.versionMajor}.{hello.versionMinor} · {hello.scenarioName}
          </span>
        ) : (
          <span className="meta">no Hello yet</span>
        )}
        <span className="spacer grow" />
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
            <button type="button" className={tab === "compare" ? "active" : ""} onClick={() => setTab("compare")} data-testid="tab-compare">
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
          <div className={compare === null ? "viewstack" : "viewstack split"} data-testid="viewstack">
            <Viewport />
            {compare === null ? null : <ComparePane />}
          </div>
          <TimeControls />
        </main>

        <aside className="panel right">
          <Inspector />
        </aside>
      </div>

      <PlotsStrip />

      {connectError ? (
        <div className="note err" style={{ position: "fixed", bottom: 12, right: 12, maxWidth: 480, zIndex: 100 }}>
          Could not reach an engine: {connectError}. Start the real one with{" "}
          <code>cargo run -p v2xw-server -- --scenario scenarios/… --port 8787</code>, its fixture engine with{" "}
          <code>cargo run -p v2xw-server -- --actors 200 --port 8787</code>, or the TypeScript mock with{" "}
          <code>node packages/mock-server/dist/index.js --actors 200 --port 8787</code>.
        </div>
      ) : null}
    </div>
  );
}
