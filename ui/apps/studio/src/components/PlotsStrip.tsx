/**
 * The plots strip of the 09-ui §6 wireframe: "Plots: [PDR vs distance] [CBR vs time] …".
 *
 * Series come from the `MetricSample` frames (§3.7) as they arrive — the same samples the engine
 * would return from `metrics.query` — so the strip is live with no polling. The catalogue behind
 * the `+` button is `metrics.query` with no `metrics` argument (§6.12), which returns the metric
 * definitions with their units and visibility. Clicking a plot's title opens the "why" tab for that
 * metric, resolving its `prov_id` through the `Provenance` frames (§3.8).
 */

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import uPlot from "uplot";

import { engine } from "../state/engine.js";
import { useStudio } from "../state/store.js";
import { metricSubject } from "../lib/provenance.js";

const PLOT_W = 250;
const PLOT_H = 86;

/** One catalogue row of `metrics.query` with no `metrics` argument (§6.12). */
interface MetricDefinition {
  readonly name: string;
  readonly unit: string;
  readonly visibility: string;
  readonly definition_md?: string;
}

function MetricPlot({
  name,
  tick,
  definition,
}: {
  name: string;
  tick: number;
  definition: MetricDefinition | undefined;
}): React.JSX.Element {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const plotRef = useRef<uPlot | null>(null);
  const setWhy = useStudio((s) => s.setWhy);
  const provenance = useStudio((s) => s.metricProvenance);
  const dims = useStudio((s) => s.metricDims);
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

  /**
   * The subject both the title and the value open.
   *
   * Built in one place so the two controls cannot disagree, and carrying the `prov_id` the metric's
   * own samples reported (§3.7) plus the unit from the catalogue — which is what turns the "why" tab
   * from a round trip into a local resolve (§3.8).
   */
  const subject = metricSubject(name, latest, definition?.unit, provenance[name]);
  const groundTruth = definition?.visibility === "GT";

  return (
    <div className="plot-card">
      <div className="title">
        <button
          type="button"
          className="linklike"
          title="Explain this metric (§3.8, §6.9)"
          data-testid={`plot-title-${name}`}
          aria-label={`${name}${definition?.unit ? ` in ${definition.unit}` : ""} — explain`}
          onClick={() => setWhy(subject)}
        >
          {name}
        </button>
        {groundTruth ? <span className="gt-tag">GT</span> : null}
        {/*
          The value is its own control. The title was already explainable, but the number beside it
          is the thing a reader quotes, and a number that cannot say where it came from is the rule
          this project puts first.
        */}
        <button
          type="button"
          className="linklike mono"
          data-testid={`plot-value-${name}`}
          aria-label={`${name} latest value ${latest === null ? "none" : String(latest)} — explain`}
          onClick={() => setWhy(subject)}
        >
          {latest === null ? "—" : latest.toFixed(3)}
        </button>
      </div>
      {/*
        The axes carry units, and the unit is part of the provenance: a `pdr` on a 0–1 scale and a
        `pdr` in per cent are different numbers. The catalogue's unit is shown when the engine
        published one, and its absence is shown as an absence.
      */}
      <div className="axis-note">
        <span className="faint">t (s)</span>
        <button
          type="button"
          className="linklike faint"
          data-testid={`plot-unit-${name}`}
          aria-label={`Unit of ${name}: ${definition?.unit ?? "not published by this engine"} — explain`}
          onClick={() => setWhy(subject)}
        >
          {definition?.unit ?? "unit not published"}
        </button>
        {dims[name] ? <span className="faint" title="§3.8 dimension dictionary">{dims[name]}</span> : null}
      </div>
      <div ref={hostRef} data-testid={`plot-${name}`} />
    </div>
  );
}

export function PlotsStrip(): React.JSX.Element {
  const tick = useStudio((s) => s.seriesTick);
  const connection = useStudio((s) => s.connection);
  const [selected, setSelected] = useState<string[]>([]);
  const [available, setAvailable] = useState<readonly string[]>([]);
  const [catalogue, setCatalogue] = useState<MetricDefinition[]>([]);
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

  const fetchCatalogue = useCallback(async () => {
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
  }, []);

  /**
   * Fetch the catalogue as soon as the stream is up, not when the user opens the `+` menu.
   *
   * The units and the `GT` tags come from it, and those belong on the axes from the first frame: a
   * plot whose unit appears only after someone opens a menu is a plot that was unlabelled while it
   * was being read. One call per connection (§6.12 `metrics.query` with no `metrics` argument).
   */
  useEffect(() => {
    if (connection !== "streaming") return;
    void fetchCatalogue();
  }, [connection, fetchCatalogue]);

  const byName = useMemo(() => {
    const map = new Map<string, MetricDefinition>();
    for (const row of catalogue) map.set(row.name, row);
    return map;
  }, [catalogue]);

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
        <button
          type="button"
          onClick={() => {
            setOpen((v) => !v);
            if (catalogue.length === 0) void fetchCatalogue();
          }}
          data-testid="metric-catalogue"
        >
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
          <MetricPlot key={name} name={name} tick={tick} definition={byName.get(name)} />
        ))}
      </div>
    </section>
  );
}
