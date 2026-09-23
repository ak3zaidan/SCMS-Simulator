/**
 * The header's "Details" disclosure: everything the old title bar used to shout.
 *
 * Before this, one line of the header carried eleven facts — the product name, the connection
 * token, the engine implementation, the engine build string, the protocol version, the scenario
 * name, four frame-rate figures, the run id and the actor count — with two 64-character digests a
 * click away in a panel that wrapped them across two lines. It read as a diagnostic dump, and the
 * two facts a user actually needs continuously (is it running, and where is the clock) were the
 * hardest to find on it.
 *
 * Everything is still here, in groups, one click away, and every identifier copies in full. What
 * the header keeps is the state, the scenario, the clock and the connection.
 */

import { useEffect, useRef, useState } from "react";

import { EngineTargetChip } from "./EngineTarget.js";
import { Identifier } from "./Identifier.js";
import { StatsReadout } from "./StatsReadout.js";
import { useStudio } from "../state/store.js";
import { durationNs, int, simClock } from "../lib/format.js";

/**
 * Split an engine's own version string into a version and a build digest.
 *
 * Engines report themselves as `v2xw 0.1.0 (c3d2242…49d)`, and that parenthesised commit is 40
 * characters of hex. Printed whole it wrapped onto a second line of the panel and pushed the
 * version — the part anyone reads — out of the way. Split, the version reads as a version and the
 * commit copies in one click.
 */
function splitBuild(version: string): { readonly name: string; readonly build: string } {
  const match = /^(.*?)\s*\(([0-9a-fA-F]{7,64})\)\s*$/.exec(version);
  if (!match) return { name: version, build: "" };
  return { name: match[1], build: match[2] };
}

export function RunDetails(): React.JSX.Element {
  const [open, setOpen] = useState(false);
  const hello = useStudio((s) => s.hello);
  const run = useStudio((s) => s.run);
  const frames = useStudio((s) => s.frames);
  const connection = useStudio((s) => s.connection);
  const target = useStudio((s) => s.target);
  const devDetails = useStudio((s) => s.devDetails);
  const setDevDetails = useStudio((s) => s.setDevDetails);
  const ref = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!open) return;
    const onDown = (ev: MouseEvent): void => {
      if (ref.current && !ref.current.contains(ev.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [open]);

  return (
    <div className="menu" ref={ref}>
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
        data-testid="run-details-button"
        title="Run identity, engine build, timing and stream counters"
      >
        Details {open ? "▴" : "▾"}
      </button>
      {open ? (
        <div className="menu-pop anchor-right wide" data-testid="run-details">
          <div className="sec">Engine</div>
          <div className="row">
            <EngineTargetChip onRetarget={() => setOpen(false)} />
            <span className="dim">{target.engine}</span>
          </div>
          <dl className="kv">
            <dt>address</dt>
            <dd>{target.baseUrl === "" ? window.location.origin : target.baseUrl}</dd>
            <dt>version</dt>
            <dd data-testid="engine-version">
              {hello ? splitBuild(hello.engineVersion).name : "—"}
            </dd>
            {hello && splitBuild(hello.engineVersion).build !== "" ? (
              <>
                <dt>built from</dt>
                <dd>
                  <Identifier
                    value={splitBuild(hello.engineVersion).build}
                    label="engine build commit"
                    chars={7}
                    testId="detail-engine-build"
                  />
                </dd>
              </>
            ) : null}
            <dt>stream</dt>
            <dd>
              {connection}
              {hello ? ` · protocol ${hello.versionMajor}.${hello.versionMinor}` : ""}
            </dd>
          </dl>

          <div className="sec">This run</div>
          <dl className="kv">
            <dt>scenario</dt>
            <dd>{hello?.scenarioName ?? "—"}</dd>
            <dt>label</dt>
            <dd>{hello?.runLabel || "—"}</dd>
            <dt>run id</dt>
            <dd>
              <Identifier value={run.runId || hello?.runId || ""} label="run id" testId="detail-run-id" />
            </dd>
            <dt>clock</dt>
            <dd>
              {simClock(run.tNs)} of {durationNs(run.tEndNs > 0 ? run.tEndNs : hello?.simDurationNs ?? 0)}
            </dd>
            <dt>vehicles / radios</dt>
            <dd>
              {int(run.actors)} / {int(run.nodes)}
            </dd>
            <dt>speed</dt>
            <dd>{run.speed}× real time</dd>
          </dl>

          <div className="sec">Identity of what is on screen</div>
          <dl className="kv">
            <dt>world</dt>
            <dd>
              <Identifier value={hello?.worldHash ?? ""} label="world digest" testId="detail-world-hash" />
            </dd>
            <dt>scenario</dt>
            <dd>
              <Identifier value={hello?.scenarioHash ?? ""} label="scenario digest" testId="detail-scenario-hash" />
            </dd>
          </dl>
          <p className="help">
            Two runs computed on the same streets and the same settings carry the same pair of digests.
            Click either to copy it in full.
          </p>

          <div className="sec">Drawing and stream</div>
          <div className="dim">
            <StatsReadout />
          </div>
          <dl className="kv">
            <dt>snapshots</dt>
            <dd>
              {int(frames.keyframe)} full, {int(frames.delta)} incremental
            </dd>
            <dt>updates</dt>
            <dd>
              {int(frames.telemetry)} radio, {int(frames.event)} event, {int(frames.metric)} measurement
            </dd>
            <dt>timing</dt>
            <dd>
              {hello ? `${durationNs(hello.mobilityStepNs)} per step · full snapshot every ${durationNs(hello.keyframePeriodNs)}` : "—"}
            </dd>
          </dl>

          <div className="sec">For debugging</div>
          <label className="check">
            <input
              type="checkbox"
              checked={devDetails}
              onChange={(e) => setDevDetails(e.target.checked)}
              data-testid="dev-details-toggle"
            />
            <span>Show protocol names and specification references</span>
          </label>
          <p className="help">
            Adds the wire field names, the method behind each control and the specification section each
            model follows, wherever the interface explains a value. Off by default: the explanations read
            in plain language without them.
          </p>
        </div>
      ) : null}
    </div>
  );
}
