/**
 * The plots strip of the 09-ui §6 wireframe: "Plots: [PDR vs distance] [CBR vs time] …".
 *
 * Series come from the `MetricSample` frames (§3.7) as they arrive — the same samples the engine
 * would return from `metrics.query` — so the strip is live with no polling. The catalogue behind
 * the `+` button is `metrics.query` with no `metrics` argument (§6.12), which returns the metric
 * definitions with their units and visibility. Clicking a plot's title opens the "why" tab for that
 * metric, resolving its `prov_id` through the `Provenance` frames (§3.8).
 */

import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import uPlot from "uplot";

import { engine } from "../state/engine.js";
import { useStudio } from "../state/store.js";

const PLOT_W = 250;
const PLOT_H = 86;

function MetricPlot({ name, tick }: { name: string; tick: number }): React.JSX.Element {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const plotRef = useRef<uPlot | null>(null);
  const setWhy = useStudio((s) => s.setWhy);
  const theme = useStudio((s) => s.theme);
  const [latest, setLatest] = useState<number | null>(null);

  useLayoutEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    const css = getComputedStyle(document.documentElement);
    const stroke = css.getPropertyValue("--accent").trim() || "#56b4e9";
    const grid = css.getPropertyValue("--border").trim() || "#223041";
    const text = css.getPropertyValue("--text-dim").trim() || "#8fa3b8";
    const plot = new uPlot(
      {
        width: PLOT_W,
        height: PLOT_H,
        legend: { show: false },
        cursor: { show: true, drag: { x: false, y: false } },
        scales: { x: { time: false } },
        axes: [
          { stroke: text, grid: { stroke: grid, width: 1 }, size: 22, font: "9px system-ui" },
          { stroke: text, grid: { stroke: grid, width: 1 }, size: 34, font: "9px system-ui" },
        ],
        series: [{ label: "t (s)" }, { label: name, stroke, width: 1.4, points: { show: false } }],
      },
      [[], []] as unknown as uPlot.AlignedData,
      host,
    );
    plotRef.current = plot;
    return () => {
      plot.destroy();
      plotRef.current = null;
    };
  }, [name, theme]);

  useEffect(() => {
    const plot = plotRef.current;
    if (!plot) return;
    plot.setData(engine.metrics.get(name) as unknown as uPlot.AlignedData);
    setLatest(engine.metrics.latest(name));
  }, [tick, name]);

  return (
    <div className="plot-card">
      <div className="title">
        <button
          type="button"
          className="linklike"
          title="Explain this metric (§6.9)"
          data-testid={`plot-title-${name}`}
          onClick={() => setWhy({ kind: "metric", id: name, label: name, value: latest === null ? undefined : String(latest) })}
        >
          {name}
        </button>
        <span className="mono">{latest === null ? "—" : latest.toFixed(3)}</span>
      </div>
      <div ref={hostRef} data-testid={`plot-${name}`} />
    </div>
  );
}

export function PlotsStrip(): React.JSX.Element {
  const tick = useStudio((s) => s.seriesTick);
  const [selected, setSelected] = useState<string[]>([]);
  const [available, setAvailable] = useState<readonly string[]>([]);
  const [catalogue, setCatalogue] = useState<{ name: string; unit: string; visibility: string; definition_md?: string }[]>([]);
  const [open, setOpen] = useState(false);
  const seenVersion = useRef(-1);

  useEffect(() => {
    // `MetricHistory.seriesVersion` moves only when a series appears or is evicted, so the name
    // list is rebuilt then and not on every 5 Hz tick. Comparing `names().length` (as this did)
    // also missed a renamed metric at a constant count.
    const version = engine.metrics.seriesVersion;
    if (version === seenVersion.current) return;
    seenVersion.current = version;
    const names = engine.metrics.names();
    setAvailable(names);
    if (selected.length === 0 && names.length > 0) setSelected(names.slice(0, 5));
  }, [tick, selected.length]);

  const loadCatalogue = useCallback(async () => {
    setOpen((v) => !v);
    if (catalogue.length > 0) return;
    try {
      const res = await engine.request("metrics.query", {});
      setCatalogue(
        (res.catalogue ?? []).map((c) => ({
          name: c.name,
          unit: c.unit,
          visibility: c.visibility,
          ...(c.definition_md ? { definition_md: c.definition_md } : {}),
        })),
      );
    } catch {
      setCatalogue([]);
    }
  }, [catalogue.length]);

  return (
    <section className="plots" data-testid="plots-strip">
      <div className="plots-head">
        <span className="dim">Plots</span>
        {available.map((name) => (
          <button
            key={name}
            type="button"
            className={selected.includes(name) ? "active" : ""}
            onClick={() => setSelected((s) => (s.includes(name) ? s.filter((n) => n !== name) : [...s, name]))}
          >
            {name}
          </button>
        ))}
        <button type="button" onClick={() => void loadCatalogue()} data-testid="metric-catalogue">
          +
        </button>
        {open ? (
          <span className="dim mono" style={{ whiteSpace: "nowrap" }}>
            {catalogue.length > 0
              ? catalogue.map((c) => `${c.name} [${c.unit}${c.visibility === "GT" ? " · GT" : ""}]`).join("  ·  ")
              : "metrics.query returned no catalogue"}
          </span>
        ) : null}
      </div>
      <div className="plots-body">
        {selected.length === 0 ? (
          <p className="dim" style={{ margin: 4 }}>
            No <code>MetricSample</code> frames yet (§3.7). They arrive every{" "}
            <code>metric_period_ns</code> once the run is streaming.
          </p>
        ) : null}
        {selected.map((name) => (
          <MetricPlot key={name} name={name} tick={tick} />
        ))}
      </div>
    </section>
  );
}
