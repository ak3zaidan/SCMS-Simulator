/**
 * The Studio shell: scenario and setup on the left, the viewport in the centre with the time
 * controls under it, the inspector on the right, and the plots strip across the bottom.
 *
 * # The header
 *
 * It carries four things, and only four: what the run is doing, which scenario it is, where the
 * simulated clock is, and the one button worth pressing next. Everything else the header used to
 * carry — the engine build string, the protocol version, the run id, the frame-rate figures, the
 * two 64-character digests — moved into the Details disclosure, which is a click away and copies
 * each identifier in full. Eleven facts on one line is a diagnostic readout, not a title bar.
 *
 * # Saying what is happening
 *
 * Every state the connection and the run can be in gets a sentence and, where one helps, the
 * button that resolves it (`lib/status.ts`, `components/Status.tsx`). The case that drove this: a
 * run reaching its end used to leave the word "closed" beside a Connect button that appeared dead,
 * while the engine was answering `state: "finished"` on a transport the page had stopped using. It
 * now says the run finished and offers to run it again — and that button works from a closed page,
 * because `engine.startRun()` falls back to HTTP.
 *
 * # Two things not in the original sketch
 *
 *  * **The engine target.** The Studio does not assume the fixture. On mount it resolves a base URL
 *    with `lib/target.ts` and the Details panel says which implementation answered. That readout is
 *    the point: a plot of fixture values and a plot of simulation results must not look the same.
 *  * **The comparison pane.** With side B open the centre splits in two, one `Viewer` each, and the
 *    time controls underneath drive both.
 */

import { Fragment, useCallback, useEffect, useState } from "react";

import { ComparePane } from "./components/ComparePane.js";
import { ComparisonView } from "./components/ComparisonView.js";
import { CopilotPanel } from "./components/CopilotPanel.js";
import { HeaderClock } from "./components/HeaderClock.js";
import { Inspector } from "./components/Inspector.js";
import { PlotsStrip } from "./components/PlotsStrip.js";
import { RunBrowser } from "./components/RunBrowser.js";
import { RunDetails } from "./components/RunDetails.js";
import { ScenarioPanel } from "./components/ScenarioPanel.js";
import { PrimaryAction, StatusBanner, StatusPill } from "./components/Status.js";
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

/**
 * The left-hand tabs, in the order the work happens, with the two occasional ones set apart.
 *
 * The four used to carry identical weight in an order nothing motivated, so nothing on the page
 * said which to open first — and the answer is not a matter of taste: you cannot compare two runs
 * before producing one, and the assistant panel is a reference. Scenario is first and is the
 * default because it is where a run is started; Runs is next because it is where the run you
 * started, and any recording of one, is looked at; Compare and Copilot come after a rule, because
 * they are for when you already have something.
 *
 * The labels are what each tab holds, and the hint is what it is *for*, which is the part a first
 * visit needs. `primary: false` puts a tab after the rule.
 */
const TABS: readonly { name: LeftTab; label: string; hint: string; primary: boolean }[] = [
  { name: "scenario", label: "Scenario", hint: "Start here: the settings a run is started from, and the Run button", primary: true },
  { name: "runs", label: "Runs", hint: "The run on screen, recordings opened from files, and the world under them", primary: true },
  { name: "compare", label: "Compare", hint: "Put a second run or a recording beside this one, on one clock", primary: false },
  { name: "copilot", label: "Commands", hint: "Every command this engine accepts, and every one this page has sent", primary: false },
];

export function App(): React.JSX.Element {
  const [tab, setTab] = useState<LeftTab>("scenario");
  const theme = useStudio((s) => s.theme);
  const setTheme = useStudio((s) => s.setTheme);
  const compare = useStudio((s) => s.compare);
  const recordingCount = useStudio((s) => s.recordings.length);
  const scenarioName = useStudio((s) => s.hello?.scenarioName ?? null);
  const [connectError, setConnectError] = useState<string | null>(null);

  /**
   * Resolve a target, then connect to it.
   *
   * Kept as one function because the two are one decision: connecting to an engine the app has not
   * identified is how the Studio ended up silently pinned to the fixture. The probe is cheap — one
   * `GET /healthz` per candidate, in sequence, with a short timeout — and its result is what the
   * Details panel reports.
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
        <h1>V2X Simulator</h1>
        <StatusPill />
        <span className="topbar-scenario" data-testid="header-scenario">
          {scenarioName ?? <span className="faint">no scenario loaded</span>}
        </span>
        <HeaderClock />
        <span className="spacer grow" />
        <PrimaryAction onConnect={reconnect} />
        <RunDetails />
        <button type="button" onClick={toggleTheme} data-testid="theme-toggle" title="Switch between the dark and light palette">
          {theme === "dark" ? "Light" : "Dark"}
        </button>
      </header>

      <div className="body">
        <aside className="panel left">
          <div className="panel-head tabs">
            {TABS.map((t, i) => (
              <Fragment key={t.name}>
                {/* The rule between the two you need and the two you might. */}
                {i > 0 && TABS[i - 1].primary && !t.primary ? <span className="tab-rule" aria-hidden="true" /> : null}
                <button
                  type="button"
                  className={tab === t.name ? "active" : ""}
                  onClick={() => setTab(t.name)}
                  title={t.hint}
                  aria-current={tab === t.name ? "page" : undefined}
                  {...(t.name === "compare" ? { "data-testid": "tab-compare" } : {})}
                >
                  {t.label}
                  {t.name === "runs" && recordingCount > 0 ? (
                    <span className="tab-count" title={`${recordingCount} recording${recordingCount === 1 ? "" : "s"} open in this page`}>
                      {recordingCount}
                    </span>
                  ) : null}
                  {t.name === "compare" && compare !== null ? (
                    <span className="tab-count" title="A second run is open beside this one">
                      B
                    </span>
                  ) : null}
                </button>
              </Fragment>
            ))}
          </div>
          {tab === "scenario" ? <ScenarioPanel /> : null}
          {tab === "runs" ? <RunBrowser /> : null}
          {tab === "compare" ? <ComparisonView /> : null}
          {tab === "copilot" ? <CopilotPanel /> : null}
        </aside>

        <main className="centre">
          <StatusBanner onConnect={reconnect} note={connectError} />
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

    </div>
  );
}
