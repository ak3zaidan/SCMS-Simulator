/**
 * Which engine the Studio is talking to, and how to change it.
 *
 * `crates/v2xw-server` and `ui/packages/mock-server` serve the same four routes, so the target is a
 * base URL and nothing else. `lib/target.ts` resolves it — preferring a real engine over the
 * fixture when both answer — and this is the readout and the override.
 *
 * The flavour is shown because it changes what the numbers mean. The fixture engine's telemetry is
 * a deterministic synthetic (`crates/v2xw-server/src/stub.rs`: "Every number it reports is a
 * fixture value"), so a plot drawn from it is a plot of the fixture, and a reader who cannot tell
 * which engine answered cannot tell the difference. That is the same rule the model cards follow.
 */

import { useCallback, useState } from "react";

import { engine } from "../state/engine.js";
import { useStudio } from "../state/store.js";
import { clearOverride, describeProbe, normaliseBase, writeOverride } from "../lib/target.js";

const FLAVOUR_CLASS: Record<string, string> = { real: "ok", mock: "warn", unknown: "" };

const FLAVOUR_LABEL: Record<string, string> = {
  real: "v2xw-server",
  mock: "fixture engine",
  unknown: "unidentified engine",
};

/**
 * @param onRetarget re-resolves the target and reconnects. It takes no argument on purpose: the pin
 * is written to storage first, and re-running the whole resolve is what keeps the chip's reported
 * flavour and the connection from disagreeing — a pin that reconnected without re-probing would
 * leave "fixture engine" on screen while the real one was streaming.
 */
export function EngineTargetChip({ onRetarget }: { onRetarget: () => void }): React.JSX.Element {
  const target = useStudio((s) => s.target);
  const [open, setOpen] = useState(false);
  const [draft, setDraft] = useState("");

  const pin = useCallback(
    (baseUrl: string) => {
      writeOverride(normaliseBase(baseUrl), typeof localStorage === "undefined" ? null : localStorage);
      setOpen(false);
      onRetarget();
    },
    [onRetarget],
  );

  const unpin = useCallback(() => {
    clearOverride(typeof localStorage === "undefined" ? null : localStorage);
    setOpen(false);
    onRetarget();
  }, [onRetarget]);

  return (
    <div className="menu">
      <button
        type="button"
        className={`pill ${FLAVOUR_CLASS[target.flavour] ?? ""}`}
        onClick={() => setOpen((v) => !v)}
        data-testid="engine-target"
        title={describeProbe({
          baseUrl: target.baseUrl,
          reachable: target.reachable,
          flavour: target.flavour,
          engine: target.engine,
          runs: [],
          status: 0,
        })}
      >
        {FLAVOUR_LABEL[target.flavour] ?? target.flavour}
        {target.pinned ? " · pinned" : ""}
      </button>
      {open ? (
        <div className="menu-pop wide" data-testid="engine-target-menu">
          <div className="sec">Target</div>
          <dl className="kv">
            <dt>base URL</dt>
            <dd>{target.baseUrl === "" ? window.location.origin : target.baseUrl}</dd>
            <dt>banner</dt>
            <dd>{target.engine}</dd>
            <dt>reachable</dt>
            <dd>{target.reachable ? "yes" : "no"}</dd>
          </dl>
          {target.flavour === "mock" ? (
            <div className="note">
              This is <code>@vwp/mock-server</code>. Every telemetry and metric value it reports is a
              deterministic fixture, not a simulation result.
            </div>
          ) : null}
          {target.worldBlocked ? (
            <div className="note err">
              The engine is on another origin, and §1.1 makes both servers set{" "}
              <code>cross-origin-resource-policy: same-origin</code>, so the world payload cannot be fetched
              from this page. The stream still works; put the engine behind the dev proxy
              (<code>VWP_ENGINE</code>) or serve the Studio from the engine.
            </div>
          ) : null}
          <div className="sec">Probed</div>
          {target.tried.map((probe) => (
            <div className="row" key={probe.baseUrl === "" ? "origin" : probe.baseUrl}>
              <button type="button" className="linklike" onClick={() => pin(probe.baseUrl)}>
                {probe.baseUrl === "" ? "this origin" : probe.baseUrl}
              </button>
              <span className={probe.reachable ? "dim" : "faint"}>
                {probe.reachable ? `${probe.flavour} · ${probe.engine}` : probe.engine}
              </span>
            </div>
          ))}
          <div className="sec">Pin another</div>
          <div className="row">
            <input
              type="text"
              value={draft}
              placeholder="http://127.0.0.1:8787"
              onChange={(e) => setDraft(e.target.value)}
              aria-label="Engine base URL"
            />
            <button type="button" disabled={draft.trim() === ""} onClick={() => pin(draft)} data-testid="engine-pin">
              pin
            </button>
            {target.pinned ? (
              <button type="button" onClick={unpin} data-testid="engine-unpin">
                un-pin
              </button>
            ) : null}
          </div>
          <div className="row">
            <button type="button" onClick={() => void engine.refreshStatus()}>
              refresh status
            </button>
          </div>
        </div>
      ) : null}
    </div>
  );
}
