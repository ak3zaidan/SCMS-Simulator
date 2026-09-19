/**
 * The actor-state legend (09-ui §10): a colour-blind-safe palette with shape redundancy.
 *
 * Both the colour and the shape come from `@vwp/viewer` — the colours are its Okabe–Ito
 * `theme.actorState` entries and the shapes are the marker geometries `StateMarkerOverlay` draws —
 * so the key in the DOM is the key to the scene, not an approximation of it.
 */

import { useStudio } from "../state/store.js";
import { actorStatePalette } from "../lib/theme.js";

function Glyph({ shape, color }: { shape: string; color: string }): React.JSX.Element {
  const common = { fill: color, stroke: color, strokeWidth: 1.4 } as const;
  return (
    <svg width="12" height="12" viewBox="-6 -6 12 12" aria-hidden="true" focusable="false">
      {shape === "circle" ? <circle r="4" {...common} /> : null}
      {shape === "triangle" ? <polygon points="0,-5 4.5,4 -4.5,4" {...common} /> : null}
      {shape === "diamond" ? <polygon points="0,-5 5,0 0,5 -5,0" {...common} /> : null}
      {shape === "cross" ? (
        <path d="M-4.5,-4.5 L4.5,4.5 M4.5,-4.5 L-4.5,4.5" fill="none" stroke={color} strokeWidth="2.2" />
      ) : null}
      {shape === "ring" ? <circle r="4" fill="none" stroke={color} strokeWidth="2" /> : null}
    </svg>
  );
}

export function StateLegend(): React.JSX.Element {
  const theme = useStudio((s) => s.theme);
  const palette = actorStatePalette(theme);
  return (
    <div
      className="chip legend"
      data-testid="state-legend"
      style={{ position: "absolute", right: 8, top: 46, flexDirection: "column", alignItems: "flex-start", gap: 2 }}
    >
      {palette.map((p) => (
        <span className="item" key={p.key}>
          <Glyph shape={p.shape} color={p.color} />
          {p.label}
        </span>
      ))}
    </div>
  );
}
