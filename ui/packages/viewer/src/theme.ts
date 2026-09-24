/**
 * Palettes and render tuning. 09-ui §10 asks for light and dark themes and a colour-blind-safe
 * categorical palette for the four actor states with shape redundancy; `overlays.ts` supplies the
 * redundant marker shapes and the state colours below are the palette.
 *
 * ## What "colour-blind-safe" means here, precisely
 *
 * Every entry of `actorState` is one of three things, and `test/theme.test.ts` asserts it:
 *
 * 1. a colour from the Okabe–Ito qualitative set (Okabe & Ito 2008) exactly;
 * 2. an Okabe–Ito colour scaled in *linear* sRGB — same hue and saturation, lower luminance —
 *    because the set is designed for a white background and some of it has to be darkened to clear
 *    the WCAG 2.1 SC 1.4.11 non-text contrast floor of 3:1 against the light theme's own
 *    background;
 * 3. a near-neutral grey, for `benign` and `selected`, which are deliberately *not* categorical
 *    hues: benign is "nothing to report" and selected is a transient highlight.
 *
 * On top of that the test simulates the three dichromacies (Viénot 1999, in linear sRGB through
 * HPE LMS) and requires a pairwise CIEDE2000 distance of at least 15 between the four state
 * colours under every one of them, plus 3:1 contrast against the theme background. A palette that
 * merely *comes from* Okabe–Ito is not automatically safe once entries are darkened or a neutral is
 * added, so the property is measured rather than asserted by provenance.
 *
 * The light theme's `reported` used to be `0xb08900`, a gold that is in no palette and that
 * collapses onto `attacker` (vermillion) at ΔE00 2.9 under deuteranopia while failing the contrast
 * floor at 2.77:1; it is now the genuine Okabe–Ito blue. `revoked` is the Okabe–Ito reddish purple
 * darkened, in both themes, because a mid-lightness purple is indistinguishable from the neutral
 * `benign` grey to a protanope or a deuteranope.
 */

/** Actor state buckets that get their own colour (§3.3.4 `ActorState` bits 0–2 plus "benign"). */
export type ActorStateColorKey = "benign" | "attacker" | "reported" | "revoked" | "selected";

/** A viewer colour scheme. All colours are packed `0xRRGGBB`. */
export interface ViewerTheme {
  readonly name: string;
  readonly background: number;
  readonly fogColor: number;
  /** Metres at which fog reaches full density; 0 disables fog. */
  readonly fogFar: number;
  readonly fogNear: number;
  readonly ground: number;
  readonly road: number;
  readonly sidewalk: number;
  readonly bikeLane: number;
  readonly busLane: number;
  readonly parking: number;
  readonly junction: number;
  readonly laneMarking: number;
  readonly laneMarkingCentre: number;
  readonly crossing: number;
  readonly building: number;
  readonly buildingRoof: number;
  /** The opening drawn where a road runs through a building (`passages.ts`): the dark of a portal. */
  readonly portal: number;
  readonly water: number;
  readonly park: number;
  readonly industrial: number;
  readonly rsu: number;
  readonly signalRed: number;
  readonly signalAmber: number;
  readonly signalGreen: number;
  readonly signalDark: number;
  readonly link: number;
  readonly linkGt: number;
  readonly pulse: number;
  readonly coverage: number;
  /** Tint applied to any overlay whose data is ground truth (09-ui §6). */
  readonly groundTruthTag: number;
  readonly actorState: Readonly<Record<ActorStateColorKey, number>>;
  /** Per-category fallback when `Hello` does not give a class colour. */
  readonly actorCategory: readonly number[];
  readonly skyTop: number;
  readonly skyHorizon: number;
  readonly skyBottom: number;
}

const OKABE_ITO = {
  orange: 0xe69f00,
  skyBlue: 0x56b4e9,
  bluishGreen: 0x009e73,
  yellow: 0xf0e442,
  blue: 0x0072b2,
  vermillion: 0xd55e00,
  reddishPurple: 0xcc79a7,
} as const;

/**
 * Okabe–Ito colours scaled in linear sRGB: the same chromaticity (hue and saturation), a fraction
 * of the luminance. The suffix is that fraction, and `test/theme.test.ts` recomputes each value
 * from its parent so a typo here cannot pass as "a darkened Okabe–Ito colour".
 */
const OKABE_ITO_DARK = {
  /** `reddishPurple` at 50 % luminance — `0x95577a`. */
  reddishPurple50: 0x95577a,
  /** `reddishPurple` at 13 % luminance — `0x4f2c3f`. */
  reddishPurple13: 0x4f2c3f,
} as const;

/**
 * The default dark theme.
 *
 * ## Why the surfaces are lighter than a dark basemap would paint them
 *
 * These values are consumed as *albedo*, not as pixels: the renderer converts each one to linear
 * space, multiplies by the light rig and tone-maps the result, so a surface authored at 0x2a3138
 * left the screen at roughly 0x2b2b2b. That is a fine road on a plan view, where the only job is to
 * sit behind a bright lane marking. At street level it was the whole lower half of the frame, and a
 * building wall at 0x39424d beside it was indistinguishable from the sky — the review's "a few
 * white strips in a black void". The street-level surfaces (`ground`, `road`, `sidewalk`,
 * `junction`, `parking`, `building`, `buildingRoof`) are therefore authored for how they *render*
 * under the rig rather than for how they look in a swatch.
 *
 * `background`, `actorState` and `actorCategory` are unchanged and deliberately so: the first is
 * what `test/theme.test.ts` measures the palette's 3:1 non-text contrast against, and the second
 * two are the colour-blind-safe categorical palette the same test measures under all three
 * dichromacies. Nothing here may move them.
 */
export const DARK_THEME: ViewerTheme = {
  name: "dark",
  background: 0x0b0f14,
  fogColor: 0x0b0f14,
  fogNear: 400,
  fogFar: 3200,
  ground: 0x28323d,
  road: 0x3d454f,
  sidewalk: 0x4d5560,
  bikeLane: 0x33473d,
  busLane: 0x4a3f30,
  parking: 0x353c45,
  junction: 0x454e59,
  laneMarking: 0xb9c2cc,
  laneMarkingCentre: 0xd8c46a,
  crossing: 0xcdd6e0,
  building: 0x5b6674,
  buildingRoof: 0x424c58,
  portal: 0x0c0f13,
  water: 0x16374f,
  park: 0x24402e,
  industrial: 0x3f3b33,
  rsu: OKABE_ITO.skyBlue,
  signalRed: 0xff4d4d,
  signalAmber: 0xffb000,
  signalGreen: 0x2ecc71,
  signalDark: 0x30363c,
  link: OKABE_ITO.skyBlue,
  linkGt: OKABE_ITO.bluishGreen,
  pulse: 0x7fd4ff,
  coverage: 0x3aa0d8,
  groundTruthTag: OKABE_ITO.bluishGreen,
  actorState: {
    // Near-neutral, and deliberately the least salient thing on the map: most actors are benign.
    benign: 0x9fb4c7,
    attacker: OKABE_ITO.vermillion,
    reported: OKABE_ITO.yellow,
    // Okabe–Ito reddish purple at half luminance. At full luminance it reads as the same
    // desaturated pink-grey as `benign` to a protanope or deuteranope (ΔE00 5.6); darkened, the
    // pair separates to 23.9 and the whole palette clears 15 under every dichromacy.
    revoked: OKABE_ITO_DARK.reddishPurple50,
    selected: 0xffffff,
  },
  actorCategory: [0x8fa6bd, OKABE_ITO.orange, OKABE_ITO.blue, 0x7a8894],
  // A daylight sky, not a night one. At the default 11:00 the old values put a near-black dome
  // over a lit city: there was no horizon for a roofline to be read against, and the fog the
  // street views blend into had nothing to blend to.
  skyTop: 0x16375f,
  skyHorizon: 0x53789c,
  skyBottom: 0x121820,
};

/** The light theme, for figure export and bright rooms. */
export const LIGHT_THEME: ViewerTheme = {
  name: "light",
  background: 0xe8edf2,
  fogColor: 0xe8edf2,
  fogNear: 600,
  fogFar: 4000,
  ground: 0xdfe5eb,
  road: 0xb9c1c9,
  sidewalk: 0xcdd4db,
  bikeLane: 0xb7cfc0,
  busLane: 0xd8c9b0,
  parking: 0xc6cdd4,
  junction: 0xc2cad2,
  laneMarking: 0xfcfdfe,
  laneMarkingCentre: 0xd9a400,
  crossing: 0xffffff,
  building: 0xc4ccd5,
  buildingRoof: 0xb2bac3,
  portal: 0x2a2f36,
  water: 0x9dc6e0,
  park: 0xbcd7b6,
  industrial: 0xd3ccc0,
  rsu: OKABE_ITO.blue,
  signalRed: 0xd93025,
  signalAmber: 0xe8a000,
  signalGreen: 0x188038,
  signalDark: 0x9aa3ab,
  link: OKABE_ITO.blue,
  linkGt: OKABE_ITO.bluishGreen,
  pulse: 0x2a7fb8,
  coverage: 0x2a7fb8,
  groundTruthTag: OKABE_ITO.bluishGreen,
  actorState: {
    // Near-neutral. Slightly lighter than the old 0x4a5a6a so it clears the Okabe–Ito blue below.
    benign: 0x5a5c61,
    attacker: OKABE_ITO.vermillion,
    // Okabe–Ito blue, not the gold this used to be: on a light background the yellow end of the
    // set has no contrast to give (0xf0e442 is 1.1:1 here) and darkening it lands on vermillion.
    reported: OKABE_ITO.blue,
    // Okabe–Ito reddish purple at 13 % luminance: dark enough to separate from both the neutral
    // grey and the blue under protanopia and deuteranopia, where purple and blue coincide.
    revoked: OKABE_ITO_DARK.reddishPurple13,
    selected: 0x101418,
  },
  actorCategory: [0x53637a, OKABE_ITO.orange, OKABE_ITO.blue, 0x77828d],
  skyTop: 0x9fc4e8,
  skyHorizon: 0xd9e7f3,
  skyBottom: 0xe8edf2,
};

/** Look a theme up by name; unknown names fall back to dark. */
export function themeByName(name: string): ViewerTheme {
  return name === "light" ? LIGHT_THEME : DARK_THEME;
}
