/**
 * One sparkline of the 09-ui §5 row, drawn with uPlot.
 *
 * uPlot is the library 09-ui §5 names for exactly this: "60 fps streaming at 10 % CPU for 3,600
 * points in its published benchmark". The chart is created once, imperatively, and fed with
 * `setData` on the store's series tick — React never re-renders the canvas.
 */

import { useEffect, useLayoutEffect, useRef } from "react";
import uPlot from "uplot";

import { engine } from "../state/engine.js";
import { useStudio } from "../state/store.js";

const HEIGHT = 34;

export function Sparkline({
  seriesIndex,
  label,
  unit,
  tick,
  fieldKey,
  node,
}: {
  seriesIndex: number;
  label: string;
  unit: string;
  tick: number;
  /** The telemetry field this series is sampled from, which is what the engine is asked to explain. */
  fieldKey?: string;
  /** The followed node, so the provenance question is about a node and not about "a vehicle". */
  node?: number | null;
}): React.JSX.Element {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const plotRef = useRef<uPlot | null>(null);
  const valueRef = useRef<HTMLElement | null>(null);
  const theme = useStudio((s) => s.theme);
  const setWhy = useStudio((s) => s.setWhy);

  useLayoutEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    const stroke = getComputedStyle(document.documentElement).getPropertyValue("--accent").trim() || "#56b4e9";
    const plot = new uPlot(
      {
        width: Math.max(40, host.clientWidth),
        height: HEIGHT,
        legend: { show: false },
        cursor: { show: false },
        scales: { x: { time: false } },
        axes: [
          { show: false },
          { show: false },
        ],
        series: [
          {},
          { stroke, width: 1.25, points: { show: false }, spanGaps: false },
        ],
      },
      [[], []] as unknown as uPlot.AlignedData,
      host,
    );
    plotRef.current = plot;

    const observer = new ResizeObserver(() => {
      plot.setSize({ width: Math.max(40, host.clientWidth), height: HEIGHT });
    });
    observer.observe(host);

    return () => {
      observer.disconnect();
      plot.destroy();
      plotRef.current = null;
    };
  }, [theme]);

  useEffect(() => {
    const plot = plotRef.current;
    if (!plot) return;
    const data = engine.spark.toUplotOne(seriesIndex);
    plot.setData(data as unknown as uPlot.AlignedData);
    const latest = engine.spark.latest(seriesIndex);
    if (valueRef.current) {
      valueRef.current.textContent = latest === null ? "—" : latest >= 100 ? latest.toFixed(0) : latest.toFixed(2);
    }
  }, [tick, seriesIndex]);

  return (
    <div className="spark">
      <div className="label">
        {/*
          The sparkline is the same telemetry field the HUD shows, plotted over the retained window,
          so it resolves to the same provenance — and it has to be reachable here too, because this
          row is where the shape of a value is read rather than its latest number.
        */}
        <button
          type="button"
          className="linklike"
          title={`${label} in ${unit} — where does this come from?`}
          data-testid={`spark-why-${seriesIndex}`}
          aria-label={`${label} in ${unit} — explain`}
          onClick={() =>
            setWhy({
              kind: "node_field",
              id: fieldKey ?? label,
              label,
              ...(node === null || node === undefined ? {} : { node }),
              unit,
            })
          }
        >
          {label}
        </button>
        <b ref={valueRef} data-testid={`spark-value-${seriesIndex}`}>
          —
        </b>
      </div>
      <div ref={hostRef} data-testid={`spark-${seriesIndex}`} />
    </div>
  );
}
