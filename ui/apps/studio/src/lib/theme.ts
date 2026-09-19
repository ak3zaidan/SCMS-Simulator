/**
 * Studio theming (09-ui §10): light and dark, and a colour-blind-safe actor-state palette with
 * shape redundancy.
 *
 * The colours are *not* re-declared here. `@vwp/viewer`'s `DARK_THEME` / `LIGHT_THEME` already carry
 * the Okabe–Ito state palette that the instanced actors and the marker overlays are drawn with, and
 * `overlays.ts` already pairs each state with a distinct marker shape. The legend, the inspector
 * chips and the CSS custom properties all read those same numbers through
 * {@link actorStatePalette}, so the DOM and the WebGL scene can never drift apart.
 */

import { DARK_THEME, LIGHT_THEME, type ActorStateColorKey, type ViewerTheme } from "@vwp/viewer";

/** The two themes the Studio ships. */
export type ThemeName = "dark" | "light";

/** The viewer theme for a Studio theme name. */
export function studioTheme(name: ThemeName): ViewerTheme {
  return name === "light" ? LIGHT_THEME : DARK_THEME;
}

/** A packed `0xRRGGBB` as a CSS colour. */
export function cssHex(value: number): string {
  return `#${(value >>> 0).toString(16).padStart(6, "0")}`;
}

/**
 * The redundant shape each actor state gets, matching `StateMarkerOverlay`'s marker geometry so the
 * legend in the DOM names the same shape the viewport draws.
 */
export const STATE_SHAPES: Readonly<Record<ActorStateColorKey, "circle" | "triangle" | "diamond" | "cross" | "ring">> = {
  benign: "circle",
  attacker: "triangle",
  reported: "diamond",
  revoked: "cross",
  selected: "ring",
};

/** Human labels for the state palette, in the order 09-ui §10 lists them. */
export const STATE_LABELS: Readonly<Record<ActorStateColorKey, string>> = {
  benign: "Benign",
  attacker: "Attacker (GT)",
  reported: "Reported",
  revoked: "Revoked",
  selected: "Selected",
};

/** The state palette of a theme, as CSS colours plus their redundant shapes. */
export function actorStatePalette(name: ThemeName): {
  key: ActorStateColorKey;
  label: string;
  color: string;
  shape: (typeof STATE_SHAPES)[ActorStateColorKey];
}[] {
  const theme = studioTheme(name);
  return (Object.keys(STATE_SHAPES) as ActorStateColorKey[]).map((key) => ({
    key,
    label: STATE_LABELS[key],
    color: cssHex(theme.actorState[key]),
    shape: STATE_SHAPES[key],
  }));
}

/**
 * Publish the viewer palette to CSS custom properties, so the panels are tinted by exactly the
 * colours the scene uses. Called whenever the theme changes.
 */
export function applyThemeToDocument(name: ThemeName): void {
  const theme = studioTheme(name);
  const root = document.documentElement;
  root.dataset.theme = name;
  const vars: Record<string, number> = {
    "--state-benign": theme.actorState.benign,
    "--state-attacker": theme.actorState.attacker,
    "--state-reported": theme.actorState.reported,
    "--state-revoked": theme.actorState.revoked,
    "--state-selected": theme.actorState.selected,
    "--gt-tag": theme.groundTruthTag,
    "--signal-red": theme.signalRed,
    "--signal-amber": theme.signalAmber,
    "--signal-green": theme.signalGreen,
    "--rsu": theme.rsu,
    "--viewer-bg": theme.background,
  };
  for (const [prop, value] of Object.entries(vars)) root.style.setProperty(prop, cssHex(value));
}
