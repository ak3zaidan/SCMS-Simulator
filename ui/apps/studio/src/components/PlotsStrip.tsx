/**
 * The measurements strip: whatever the engine is measuring, plotted as it arrives.
 *
 * Series come from the engine's own metric samples as they stream in, so the strip is live with no
 * polling. What can be plotted is the engine's catalogue: one row per series the stream can carry —
 * a metric's headline, a distribution's p50/p95/p99 (`e2e_latency.p95`), and a declared breakdown
 * (`latency_stage[airtime]`, `loss_rate[collision]`). Each row carries its unit, its definition and
 * its citation, and the picker shows all of them, searchable and grouped, whether or not a sample
 * has arrived yet — so a measurement is discoverable before the run produces it, and one the run
 * never produces says so instead of silently being absent. The unit is part of the number: a
 * delivery ratio on a 0–1 scale and one in per cent are different values, and a plot whose engine
 * publishes no catalogue says so on its axis rather than inventing a unit.
 *
 * Clicking a plot's title, its value or its unit opens the "why" tab for that measurement, with the
 * model, version and parameter set that produced it.
 */

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import uPlot from "uplot";

import { engine } from "../state/engine.js";
import { useStudio } from "../state/store.js";
import { metricSubject } from "../lib/provenance.js";
import { Breakdowns } from "./Breakdowns.js";

const PLOT_W = 250;
const PLOT_H = 86;

/** One row of the engine's catalogue: a series name, its unit, its definition and where it comes from. */
interface MetricDefinition {
  readonly name: string;
  readonly unit: string;
  readonly visibility: string;
  readonly definition_md?: string;
  /** The metric this series is a view of (the name itself for a headline series). */
  readonly base: string;
  /** The citation: a standard, a paper or a design section. */
  readonly source?: string;
}

/**
 * Which family a metric belongs to, for grouping the picker the way a V2X study reads: delivery,
 * latency, awareness, channel load, overhead, then everything else. Grouping only — a metric this
 * table does not know still appears, under "Other".
 */
const FAMILIES: readonly (readonly [string, readonly string[]])[] = [
  [
    "Delivery",
    ["pdr", "per", "delivery_ratio", "pdr_by_cause", "loss_rate", "collision_rate", "half_duplex_rate", "goodput"],
  ],
  ["Latency", ["e2e_latency", "latency_stage", "latency_stage_share", "latency_trace_rejected", "mac_access_delay"]],
  ["Awareness", ["aoi", "aoi_peak", "nar", "pir"]],
  [
    "Channel load",
    [
      "cbr",
      "channel_load",
      "channel_occupancy",
      "offered_load",
      "carried_load",
      "airtime_per_node",
      "mac_queue_depth",
      "mac_drops",
    ],
  ],
  [
    "Overhead and bytes",
    [
      "security_overhead",
      "net_header_overhead",
      "link_overhead",
      "cert_bytes_share",
      "air_bytes_per_payload_byte",
      "bytes_per_vehicle_hour",
      "bytes_total",
      "bytes_air",
      "bytes_uu_ul",
      "bytes_uu_dl",
      "bytes_backhaul",
      "bytes_backend",
    ],
  ],
];

function familyOf(base: string): string {
  for (const [family, names] of FAMILIES) if (names.includes(base)) return family;
  return "Other";
}

/** What a fresh page plots first, where the run measures it: the numbers a V2X study leads with. */
const PREFERRED = ["pdr", "e2e_latency.p95", "cbr", "channel_load", "security_overhead"];

function MetricPlot({
  name,
  tick,
  definition,
  onRemove,
}: {
  name: string;
  tick: number;
  definition: MetricDefinition | undefined;
  onRemove: () => void;
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
  const groundTruth = definition?.visibility === "GT" || definition?.visibility === "NODE+GT";

  return (
    <div className="plot-card">
      <div className="title">
        <button
          type="button"
          className="linklike"
          title={definition?.definition_md ?? "Where this measurement comes from: the model, its version and its parameters"}
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
          {latest === null ? "—" : latest.toPrecision(4)}
        </button>
        <button
          type="button"
          className="linklike faint"
          aria-label={`Stop plotting ${name}`}
          title="Stop plotting this measurement"
          onClick={onRemove}
        >
          ×
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
          {definition?.unit ?? "unit not stated by this engine"}
        </button>
        {dims[name] ? (
          <span className="faint" title="The conditions this measurement was taken under">
            {dims[name]}
          </span>
        ) : null}
      </div>
      <div ref={hostRef} data-testid={`plot-${name}`} />
    </div>
  );
}

/**
 * The picker: every series the engine can measure, searchable, grouped by family, each with its
 * unit, whether data has arrived, its definition and its citation.
 */
function MetricPicker({
  catalogue,
  available,
  selected,
  toggle,
  close,
}: {
  catalogue: readonly MetricDefinition[];
  available: ReadonlySet<string>;
  selected: readonly string[];
  toggle: (name: string) => void;
  close: () => void;
}): React.JSX.Element {
  const [query, setQuery] = useState("");
  const inputRef = useRef<HTMLInputElement | null>(null);
  useEffect(() => inputRef.current?.focus(), []);

  // Rows the catalogue does not list but the stream carries (an engine without a catalogue).
  const rows = useMemo(() => {
    const byName = new Map<string, MetricDefinition>();
    for (const c of catalogue) byName.set(c.name, c);
    for (const name of available) {
      if (!byName.has(name)) byName.set(name, { name, unit: "", visibility: "", base: name });
    }
    const q = query.trim().toLowerCase();
    const matches = [...byName.values()].filter(
      (c) =>
        q === "" ||
        c.name.toLowerCase().includes(q) ||
        (c.definition_md ?? "").toLowerCase().includes(q) ||
        familyOf(c.base).toLowerCase().includes(q),
    );
    const groups = new Map<string, MetricDefinition[]>();
    for (const c of matches) {
      const family = familyOf(c.base);
      const list = groups.get(family) ?? [];
      list.push(c);
      groups.set(family, list);
    }
    const order = [...FAMILIES.map(([f]) => f), "Other"];
    return order
      .filter((f) => groups.has(f))
      .map((f) => [f, (groups.get(f) ?? []).sort((a, b) => a.name.localeCompare(b.name))] as const);
  }, [catalogue, available, query]);

  return (
    <div className="menu-pop up wide metric-picker" data-testid="metric-picker" role="dialog" aria-label="Choose measurements">
      <div className="metric-picker-head">
        <input
          ref={inputRef}
          type="search"
          placeholder="Search measurements — latency, overhead, cbr, stage…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape") close();
          }}
          data-testid="metric-search"
          aria-label="Search measurements"
        />
        <button type="button" onClick={close} aria-label="Close the measurement list">
          Done
        </button>
      </div>
      {rows.length === 0 ? (
        <p className="dim" style={{ margin: 4 }}>
          {catalogue.length === 0
            ? "This engine does not publish a list of what it measures. Whatever it sends is still plotted, but without units."
            : "Nothing matches that search."}
        </p>
      ) : null}
      {rows.map(([family, list]) => (
        <div key={family} className="metric-family">
          <div className="metric-family-name">{family}</div>
          {list.map((c) => {
            const on = selected.includes(c.name);
            const has = available.has(c.name);
            return (
              <label
                key={c.name}
                className="check metric-row"
                title={[c.definition_md, c.source ? `Source: ${c.source}` : ""].filter(Boolean).join("\n\n")}
                data-testid={`metric-option-${c.name}`}
              >
                <input type="checkbox" checked={on} onChange={() => toggle(c.name)} />
                <span className="mono">{c.name}</span>
                <span className="faint">{c.unit}</span>
                {c.visibility === "GT" || c.visibility === "NODE+GT" ? <span className="gt-tag">GT</span> : null}
                <span className={has ? "metric-live" : "faint"}>{has ? "live" : "no data yet"}</span>
              </label>
            );
          })}
        </div>
      ))}
    </div>
  );
}

export function PlotsStrip(): React.JSX.Element {
  const tick = useStudio((s) => s.seriesTick);
  const connection = useStudio((s) => s.connection);
  const running = useStudio((s) => s.run.state === "running");
  const [selected, setSelected] = useState<string[]>([]);
  const [available, setAvailable] = useState<readonly string[]>([]);
  const [catalogue, setCatalogue] = useState<MetricDefinition[]>([]);
  const [open, setOpen] = useState(false);
  const [showBreakdowns, setShowBreakdowns] = useState(true);
  const seenVersion = useRef(-1);
  // Once the user has chosen, the strip stops choosing for them.
  const userChose = useRef(false);

  useEffect(() => {
    // `MetricHistory.seriesVersion` moves only when a series appears or is evicted, so the name
    // list is rebuilt then and not on every 5 Hz tick. Comparing `names().length` (as this did)
    // also missed a renamed metric at a constant count.
    const version = engine.metrics.seriesVersion;
    if (version === seenVersion.current) return;
    seenVersion.current = version;
    const names = engine.metrics.names();
    setAvailable(names);
    if (!userChose.current && names.length > 0) {
      const preferred = PREFERRED.filter((n) => names.includes(n));
      setSelected((current) => {
        if (current.length >= 5) return current;
        const next = [...current];
        for (const n of preferred.length > 0 ? preferred : names.slice(0, 5)) {
          if (!next.includes(n) && next.length < 5) next.push(n);
        }
        return next;
      });
    }
  }, [tick]);

  const fetchCatalogue = useCallback(async () => {
    try {
      const res = await engine.request("metrics.query", {});
      setCatalogue(
        (res.catalogue ?? []).map((c) => {
          const source = c.source && typeof c.source["ref"] === "string" ? (c.source["ref"] as string) : undefined;
          return {
            name: c.name,
            unit: c.unit,
            visibility: c.visibility,
            base: c.base ?? c.name,
            ...(c.definition_md ? { definition_md: c.definition_md } : {}),
            ...(source ? { source } : {}),
          };
        }),
      );
    } catch {
      setCatalogue([]);
    }
  }, []);

  /**
   * Fetch the catalogue as soon as the stream is up, not when the user opens the picker.
   *
   * The units and the `GT` tags come from it, and those belong on the axes from the first frame: a
   * plot whose unit appears only after someone opens a menu is a plot that was unlabelled while it
   * was being read. One call per connection (§6.12 `metrics.query` with no `metrics` argument).
   */
  useEffect(() => {
    // Not gated on the stream being up. `metrics.query` is answered over HTTP as well as over the
    // socket (`StudioEngine.request` picks), so the units and the GT tags are available on a page
    // whose run has finished — which is exactly when someone is reading the plots rather than
    // watching them.
    void fetchCatalogue();
  }, [connection, fetchCatalogue]);

  const byName = useMemo(() => {
    const map = new Map<string, MetricDefinition>();
    for (const row of catalogue) map.set(row.name, row);
    return map;
  }, [catalogue]);
  const availableSet = useMemo(() => new Set(available), [available]);

  const toggle = useCallback((name: string) => {
    userChose.current = true;
    setSelected((s) => (s.includes(name) ? s.filter((n) => n !== name) : [...s, name]));
  }, []);

  const measured = catalogue.length > 0 ? catalogue.length : available.length;

  return (
    <section className="plots" data-testid="plots-strip">
      <div className="plots-head">
        <span className="dim">Measurements</span>
        <button
          type="button"
          onClick={() => {
            setOpen((v) => !v);
            if (catalogue.length === 0) void fetchCatalogue();
          }}
          data-testid="metric-catalogue"
          title="Everything this engine measures, with units, definitions and sources"
          aria-expanded={open}
        >
          {`Choose… (${selected.length} of ${measured})`}
        </button>
        <button
          type="button"
          className={showBreakdowns ? "active" : ""}
          onClick={() => setShowBreakdowns((v) => !v)}
          data-testid="breakdowns-toggle"
          title="Delivery against distance, the latency's stages and the per-node rankings, pooled over the run"
          aria-pressed={showBreakdowns}
        >
          Breakdowns
        </button>
        {selected.map((name) => (
          <span key={name} className="faint mono" style={{ whiteSpace: "nowrap" }}>
            {name}
          </span>
        ))}
      </div>
      {open ? (
        <MetricPicker
          catalogue={catalogue}
          available={availableSet}
          selected={selected}
          toggle={toggle}
          close={() => setOpen(false)}
        />
      ) : null}
      <div className="plots-body">
        {selected.length === 0 ? (
          <p className="dim" style={{ margin: 4 }} data-testid="plots-empty">
            {available.length > 0
              ? "Nothing chosen to plot. Open “Choose…” to pick a measurement."
              : connection !== "streaming"
                ? "No measurements have reached this page. They arrive over the stream, so nothing will appear here until it is open again."
                : running
                  ? "No measurements have arrived yet. The engine sends them at its own interval, usually within the first simulated second — they will appear here on their own."
                  : "No measurements yet. They arrive while a run is playing; press Run to start one."}
          </p>
        ) : null}
        {selected.map((name) => (
          <MetricPlot
            key={name}
            name={name}
            tick={tick}
            definition={byName.get(name)}
            onRemove={() => toggle(name)}
          />
        ))}
        {showBreakdowns && available.length > 0 ? <Breakdowns /> : null}
      </div>
    </section>
  );
}
