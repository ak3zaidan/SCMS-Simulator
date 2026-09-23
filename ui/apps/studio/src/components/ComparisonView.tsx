/**
 * The comparison view of 09-ui §6: "two runs side by side with synchronised time, difference
 * overlays for metrics, and manifest diff".
 *
 * This panel is the controls and the numbers; the second viewport is `ComparePane`, and the
 * synchronised scrub lives in `TimeControls`, which is where the one clock the two runs share
 * already was. What is here:
 *
 *  * **Opening side B.** Three sources, and they are genuinely different things: a second engine
 *    (everything a run has, metric samples included), a recording from a local file (no server at
 *    all — `crates/v2xw-wasm`), and a recording served over HTTP (the same reader, fetched a range
 *    at a time).
 *  * **The manifest diff**, field by field, with the differing rows marked. The two hashes are the
 *    rows that matter: two runs with different scenario hashes are not a comparison of one change.
 *  * **The metric difference**, read at one simulated instant from each side's own history, with
 *    every row opening its provenance — including the difference itself, which is computed here and
 *    says so.
 *
 * # What is *not* faked
 *
 * `metrics.query {runs: [...]}` (§6.12) and `experiment.compare` (§6.15, reserved) would make this
 * a server-side query. They are not available — see `state/compare.ts` — so the arithmetic is done
 * in the browser over two live histories, and the panel says which side each number came from. A
 * recording has no `MetricSample` frames (§7.1), and in that case the difference table says that
 * rather than showing a column of zeros.
 */

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import uPlot from "uplot";

import { alignedDiffSeries, compare } from "../state/compare.js";
import { engine } from "../state/engine.js";
import { useStudio } from "../state/store.js";
import { simClock } from "../lib/format.js";
import { differenceSubject, metricSubject } from "../lib/provenance.js";

const PLOT_W = 300;
const PLOT_H = 130;

/** How a difference is rendered: absolute, then relative when a baseline exists. */
function formatDelta(delta: number | null, relative: number | null): string {
  if (delta === null) return "—";
  const abs = Math.abs(delta) >= 1000 || (Math.abs(delta) < 1e-3 && delta !== 0) ? delta.toExponential(2) : delta.toPrecision(4);
  if (relative === null) return abs;
  return `${abs} (${(relative * 100).toFixed(1)} %)`;
}

function formatValue(v: number | null): string {
  if (v === null) return "—";
  if (Math.abs(v) >= 1000 || (Math.abs(v) < 1e-3 && v !== 0)) return v.toExponential(2);
  return v.toPrecision(4);
}

/**
 * The difference plot: side A, side B and `B − A` on side A's sample grid.
 *
 * Three series rather than one, because a difference alone hides which run moved: a delta of −0.2
 * is a different finding when A was 0.9 than when A was 0.21.
 */
function DiffPlot({ metric, tick, offsetNs }: { metric: string; tick: number; offsetNs: number }): React.JSX.Element {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const plotRef = useRef<uPlot | null>(null);
  const theme = useStudio((s) => s.theme);

  useLayoutEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    const css = getComputedStyle(document.documentElement);
    const accent = css.getPropertyValue("--accent").trim() || "#56b4e9";
    const warn = css.getPropertyValue("--warn").trim() || "#e69f00";
    const err = css.getPropertyValue("--err").trim() || "#d55e00";
    const grid = css.getPropertyValue("--border").trim() || "#223041";
    const text = css.getPropertyValue("--text-dim").trim() || "#8fa3b8";
    const plot = new uPlot(
      {
        width: PLOT_W,
        height: PLOT_H,
        legend: { show: true },
        cursor: { show: true, drag: { x: false, y: false } },
        scales: { x: { time: false } },
        axes: [
          { stroke: text, grid: { stroke: grid, width: 1 }, size: 22, font: "9px system-ui" },
          { stroke: text, grid: { stroke: grid, width: 1 }, size: 40, font: "9px system-ui" },
        ],
        series: [
          { label: "t (s)" },
          { label: "A", stroke: accent, width: 1.4, points: { show: false }, spanGaps: false },
          { label: "B", stroke: warn, width: 1.4, points: { show: false }, spanGaps: false },
          { label: "B − A", stroke: err, width: 1.6, dash: [4, 3], points: { show: false }, spanGaps: false },
        ],
      },
      [[], [], [], []] as unknown as uPlot.AlignedData,
      host,
    );
    plotRef.current = plot;
    return () => {
      plot.destroy();
      plotRef.current = null;
    };
  }, [metric, theme]);

  useEffect(() => {
    const plot = plotRef.current;
    if (!plot) return;
    const data = alignedDiffSeries(metric, engine.metrics, compare.metrics, offsetNs / 1e9);
    plot.setData(data as unknown as uPlot.AlignedData);
  }, [tick, metric, offsetNs]);

  return (
    <div className="plot-card wide" data-testid={`diff-plot-${metric}`}>
      <div className="title">
        <span>{metric}</span>
        <span className="dim">A · B · B−A</span>
      </div>
      <div ref={hostRef} />
    </div>
  );
}

export function ComparisonView(): React.JSX.Element {
  const side = useStudio((s) => s.compare);
  const sync = useStudio((s) => s.compareSync);
  const diffs = useStudio((s) => s.compareDiffs);
  const selected = useStudio((s) => s.compareMetrics);
  const setCompareSync = useStudio((s) => s.setCompareSync);
  const setCompareMetrics = useStudio((s) => s.setCompareMetrics);
  const setWhy = useStudio((s) => s.setWhy);
  const tick = useStudio((s) => s.seriesTick);
  const hello = useStudio((s) => s.hello);
  const simTimeNs = useStudio((s) => s.simTimeNs);
  const run = useStudio((s) => s.run);

  const [engineUrl, setEngineUrl] = useState("http://127.0.0.1:8788");
  const [recordingUrl, setRecordingUrl] = useState("");
  const [busy, setBusy] = useState(false);
  const [plotMetric, setPlotMetric] = useState<string | null>(null);
  const fileRef = useRef<HTMLInputElement | null>(null);

  // Units for the difference table come from side A's catalogue: `metrics.query` with no `metrics`
  // argument returns the metric definitions with their units (§6.12).
  useEffect(() => {
    let cancelled = false;
    void engine
      .request("metrics.query", {})
      .then((res) => {
        if (cancelled) return;
        const units: Record<string, string> = {};
        for (const row of res.catalogue ?? []) units[row.name] = row.unit;
        compare.setUnits(units);
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, []);

  const openEngineSide = useCallback(async () => {
    setBusy(true);
    try {
      await compare.openEngine(engineUrl, window.location.origin);
    } finally {
      setBusy(false);
    }
  }, [engineUrl]);

  const openFileSide = useCallback(async (file: File) => {
    setBusy(true);
    try {
      await compare.openRecording(file);
    } finally {
      setBusy(false);
    }
  }, []);

  const openUrlSide = useCallback(async () => {
    setBusy(true);
    try {
      await compare.openRecordingUrl(recordingUrl);
    } finally {
      setBusy(false);
    }
  }, [recordingUrl]);

  const manifest = useMemo(() => (side === null ? [] : compare.manifestRows()), [side, tick]);
  const worldMatch = side === null ? null : compare.worldMatchesA();
  const aTNs = simTimeNs > 0 ? simTimeNs : run.tNs;

  const available = useMemo(() => {
    const names = new Set<string>();
    for (const n of engine.metrics.names()) names.add(n);
    for (const n of compare.metrics.names()) names.add(n);
    return [...names].sort();
  }, [tick]);

  return (
    <div className="panel-body" data-testid="comparison-view">
      <div className="section">
        <h3>Side B</h3>
        {side === null ? (
          <p className="dim">
            Nothing to compare against yet. Open a second engine, or a recording — a recording opens with no
            engine at all, which is the path a reviewer uses to check somebody else&rsquo;s result (09-ui §7).
          </p>
        ) : (
          <dl className="kv">
            <dt>source</dt>
            <dd>{side.source === "engine" ? "second VWP connection" : "local recording (WebAssembly)"}</dd>
            <dt>label</dt>
            <dd>{side.label}</dd>
            <dt>state</dt>
            <dd className={side.state === "failed" ? "hud-missing" : undefined} data-testid="compare-state">
              {side.state}
              {side.detail === "" ? "" : ` — ${side.detail}`}
            </dd>
            <dt>span</dt>
            <dd>
              {simClock(side.startNs)} … {simClock(side.endNs)}
            </dd>
            <dt>at</dt>
            <dd>{simClock(side.tNs)}</dd>
            <dt>actors</dt>
            <dd>{side.actors}</dd>
          </dl>
        )}
        {side !== null ? (
          <div className="row">
            <button type="button" onClick={() => compare.close()} data-testid="compare-close">
              close side B
            </button>
            {compare.source === "replay" ? (
              <button type="button" onClick={() => compare.adoptWorldFromA()} title="Draw side A's world geometry in side B (§7.1: a recording carries none)">
                borrow A&rsquo;s world
              </button>
            ) : null}
          </div>
        ) : null}
      </div>

      <div className="section">
        <h3>Open</h3>
        <div className="field">
          <label htmlFor="compare-engine-url">A second engine</label>
          <div className="row">
            <input
              id="compare-engine-url"
              type="text"
              value={engineUrl}
              onChange={(e) => setEngineUrl(e.target.value)}
              placeholder="http://127.0.0.1:8788"
            />
            <button type="button" disabled={busy || engineUrl.trim() === ""} onClick={() => void openEngineSide()} data-testid="compare-open-engine">
              connect
            </button>
          </div>
          <p className="help">
            A second <code>v2xw-server</code> on another port —{" "}
            <code>cargo run -p v2xw-server -- --scenario baseline.yaml --port 8788</code>. Read-only: no{" "}
            <code>view.follow</code>, no <code>events.set</code>, so only keyframes, deltas and{" "}
            <code>MetricSample</code> come over this connection. There is no <code>--replay</code> flag on that
            binary yet (<code>serve_replay</code> is library-only), so a recording is opened below instead.
          </p>
        </div>

        <div className="field">
          <label htmlFor="compare-file">A recording, from this machine</label>
          <div className="row">
            <input
              id="compare-file"
              ref={fileRef}
              type="file"
              accept=".mcap"
              onChange={(e) => {
                const file = e.target.files?.[0];
                if (file) void openFileSide(file);
              }}
              data-testid="compare-open-file"
            />
          </div>
          <p className="help">
            Read in the page by <code>crates/v2xw-wasm</code>. The file is never uploaded, and no engine runs.
          </p>
        </div>

        <div className="field">
          <label htmlFor="compare-recording-url">A recording, over HTTP</label>
          <div className="row">
            <input
              id="compare-recording-url"
              type="text"
              value={recordingUrl}
              onChange={(e) => setRecordingUrl(e.target.value)}
              placeholder="https://…/run.mcap"
            />
            <button type="button" disabled={busy || recordingUrl.trim() === ""} onClick={() => void openUrlSide()} data-testid="compare-open-url">
              open
            </button>
          </div>
          <p className="help">
            Range-read, so only the chunks a seek touches are fetched (§7.3). The server must answer{" "}
            <code>Range</code> requests.
          </p>
        </div>
      </div>

      {side !== null ? (
        <>
          <div className="section">
            <h3>Synchronisation</h3>
            <label className="check">
              <input
                type="checkbox"
                checked={sync.time}
                onChange={(e) => setCompareSync({ time: e.target.checked })}
                data-testid="sync-time"
              />
              <span>One clock: a scrub seeks both sides</span>
            </label>
            <label className="check">
              <input
                type="checkbox"
                checked={sync.camera}
                onChange={(e) => setCompareSync({ camera: e.target.checked })}
                data-testid="sync-camera"
              />
              <span>One camera: side A&rsquo;s view is mirrored into side B</span>
            </label>
            <div className="field">
              <label htmlFor="compare-offset">
                Offset <span className="unit">B − A, seconds</span>
              </label>
              <div className="row">
                <input
                  id="compare-offset"
                  type="number"
                  step="0.1"
                  value={sync.offsetNs / 1e9}
                  onChange={(e) => {
                    const seconds = Number(e.target.value);
                    setCompareSync({ offsetNs: Number.isFinite(seconds) ? Math.round(seconds * 1e9) : 0 });
                  }}
                  data-testid="sync-offset"
                />
                <button
                  type="button"
                  title="Set the offset so side B's current time lines up with side A's"
                  onClick={() => setCompareSync({ offsetNs: side.tNs - aTNs })}
                >
                  align here
                </button>
                <button type="button" onClick={() => void compare.seekTo(aTNs)} data-testid="compare-resync">
                  resync now
                </button>
              </div>
              <p className="help">
                Zero for two runs of the same scenario, which share <code>t0</code>. Non-zero is the researcher
                asserting an alignment, and every difference below is read at <code>t</code> on A and{" "}
                <code>t + offset</code> on B.
              </p>
            </div>
          </div>

          <div className="section">
            <h3>Manifest</h3>
            {worldMatch === false ? (
              <div className="note err" data-testid="world-mismatch">
                The two sides report different <code>world_hash</code> values, so this is not one change to one
                world: every position difference below includes a geometry difference.
              </div>
            ) : null}
            {worldMatch === null && side.source === "replay" ? (
              <div className="note">
                A recording carries no <code>Hello</code> (§7.1), so there is no world hash to compare. That the
                two runs share a world is your assertion, not a checked fact.
              </div>
            ) : null}
            <table className="table" data-testid="manifest-diff">
              <thead>
                <tr>
                  <th>field</th>
                  <th>A</th>
                  <th>B</th>
                </tr>
              </thead>
              <tbody>
                {manifest.map((row) => (
                  <tr key={row.field} className={row.same ? undefined : "row-differs"}>
                    <td>{row.field}</td>
                    <td title={row.a}>{row.a.length > 18 ? `${row.a.slice(0, 16)}…` : row.a}</td>
                    <td title={row.b}>{row.b.length > 18 ? `${row.b.slice(0, 16)}…` : row.b}</td>
                  </tr>
                ))}
              </tbody>
            </table>
            {hello === null ? <p className="faint">Side A is not connected, so its column is empty.</p> : null}
          </div>

          <div className="section">
            <h3>Metric difference</h3>
            {!side.hasMetrics ? (
              <div className="note" data-testid="no-b-metrics">
                Side B reports no <code>MetricSample</code> frames.{" "}
                {side.source === "replay"
                  ? "The WebAssembly reader resolves poses and signals (crates/v2xw-wasm), not metric frames, so a recording opened here can be compared by pose and by manifest but not by metric. A second engine serving that recording would difference metrics — which needs a `--replay` flag on `v2xw-server` that does not exist yet."
                  : "Nothing has been sampled on that connection yet."}
              </div>
            ) : null}
            <p className="faint">
              Read at {simClock(aTNs)} on A and {simClock(aTNs + sync.offsetNs)} on B — the last sample at or
              before each instant, which is how a binned metric is read (§3.7).
            </p>
            <table className="table" data-testid="metric-diff">
              <thead>
                <tr>
                  <th>metric</th>
                  <th>A</th>
                  <th>B</th>
                  <th>B − A</th>
                </tr>
              </thead>
              <tbody>
                {diffs.map((row) => (
                  <tr key={row.metric} className={row.oneSided ? "row-one-sided" : undefined}>
                    <td>
                      <button
                        type="button"
                        className="linklike"
                        data-testid={`diff-metric-${row.metric}`}
                        onClick={() => setPlotMetric(row.metric)}
                        title="Plot A, B and the difference"
                      >
                        {row.metric}
                      </button>
                      {row.unit === "" ? null : <span className="faint"> {row.unit}</span>}
                      {row.oneSided ? <span className="gt-tag" title="Only one side reports this metric at all">1-sided</span> : null}
                    </td>
                    <td>
                      <button
                        type="button"
                        className="linklike"
                        onClick={() => setWhy(metricSubject(row.metric, row.a, row.unit, useStudio.getState().metricProvenance[row.metric]))}
                        aria-label={`${row.metric} on side A: ${formatValue(row.a)} — explain`}
                      >
                        {formatValue(row.a)}
                      </button>
                    </td>
                    <td>{formatValue(row.b)}</td>
                    <td>
                      <button
                        type="button"
                        className="linklike"
                        data-testid={`diff-why-${row.metric}`}
                        onClick={() => setWhy(differenceSubject(row.metric, row.delta, row.unit))}
                        aria-label={`${row.metric} difference: ${formatDelta(row.delta, row.relative)} — explain`}
                      >
                        {formatDelta(row.delta, row.relative)}
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
            {diffs.length === 0 ? <p className="dim">No metric has been sampled on either side yet.</p> : null}
          </div>

          <div className="section">
            <h3>Rows shown</h3>
            <div className="chips">
              <button
                type="button"
                className={selected.length === 0 ? "active" : ""}
                onClick={() => setCompareMetrics([])}
                data-testid="diff-metrics-all"
              >
                every metric
              </button>
              {available.map((name) => (
                <button
                  key={name}
                  type="button"
                  className={selected.includes(name) ? "active" : ""}
                  onClick={() =>
                    setCompareMetrics(selected.includes(name) ? selected.filter((n) => n !== name) : [...selected, name])
                  }
                >
                  {name}
                </button>
              ))}
            </div>
          </div>

          {plotMetric !== null ? (
            <div className="section">
              <h3>
                {plotMetric}{" "}
                <button type="button" className="linklike" onClick={() => setPlotMetric(null)}>
                  close
                </button>
              </h3>
              <DiffPlot metric={plotMetric} tick={tick} offsetNs={sync.offsetNs} />
            </div>
          ) : null}
        </>
      ) : null}
    </div>
  );
}
