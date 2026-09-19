/**
 * Colour-vision-deficiency simulation and perceptual distance, for the palette tests.
 *
 * 09-ui §10 requires "a colour-blind-safe categorical palette for actor states (benign, attacker,
 * reported, revoked) with shape redundancy", and `theme.ts` claims the state colours are Okabe–Ito.
 * A claim like that is testable, so this is the instrument that tests it:
 *
 * - dichromat simulation by the Viénot–Brettel–Mollon 1999 method, in **linear** sRGB through
 *   Hunt–Pointer–Estevez LMS, which is the same construction the review used;
 * - CIEDE2000 for perceptual distance, because sRGB and even CIE76 distances misrank blues badly;
 * - WCAG 2.1 relative luminance and contrast ratio, for SC 1.4.11 non-text contrast (3:1).
 *
 * Nothing here is used at runtime; it exists so the palette cannot regress silently.
 */

/** The three dichromacies. */
export type Dichromacy = "protanopia" | "deuteranopia" | "tritanopia";

/** All four vision models the palette is checked under. */
export const VISION_MODELS = ["normal", "protanopia", "deuteranopia", "tritanopia"] as const;
export type VisionModel = (typeof VISION_MODELS)[number];

/** Unpack `0xRRGGBB` into sRGB components in `[0, 1]`. */
export function unpack(hex: number): [number, number, number] {
  return [((hex >> 16) & 0xff) / 255, ((hex >> 8) & 0xff) / 255, (hex & 0xff) / 255];
}

/** Pack sRGB components in `[0, 1]` back into `0xRRGGBB`. */
export function pack(rgb: readonly [number, number, number]): number {
  const b = (v: number): number => Math.max(0, Math.min(255, Math.round(v * 255)));
  return (b(rgb[0]) << 16) | (b(rgb[1]) << 8) | b(rgb[2]);
}

/** `#rrggbb`, for readable test output. */
export function css(hex: number): string {
  return `#${(hex >>> 0).toString(16).padStart(6, "0")}`;
}

function toLinear(c: number): number {
  return c <= 0.04045 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4);
}

function toSrgb(c: number): number {
  const v = Math.max(0, Math.min(1, c));
  return v <= 0.0031308 ? v * 12.92 : 1.055 * Math.pow(v, 1 / 2.4) - 0.055;
}

/** WCAG 2.1 relative luminance of a packed sRGB colour. */
export function luminance(hex: number): number {
  const [r, g, b] = unpack(hex).map(toLinear) as [number, number, number];
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

/** WCAG 2.1 contrast ratio between two packed sRGB colours, ≥ 1. */
export function contrastRatio(a: number, b: number): number {
  const la = luminance(a);
  const lb = luminance(b);
  return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
}

// Hunt–Pointer–Estevez LMS, normalised to D65, as used by Viénot et al. 1999.
const RGB_TO_LMS = [
  0.31399022, 0.63951294, 0.04649755,
  0.15537241, 0.75789446, 0.08670142,
  0.01775239, 0.10944209, 0.87256922,
];
const LMS_TO_RGB = [
  5.47221206, -4.64196010, 0.16963708,
  -1.12524190, 2.29317094, -0.16789520,
  0.02980165, -0.19318073, 1.16364789,
];

function mul3(m: readonly number[], v: readonly [number, number, number]): [number, number, number] {
  return [
    m[0] * v[0] + m[1] * v[1] + m[2] * v[2],
    m[3] * v[0] + m[4] * v[1] + m[5] * v[2],
    m[6] * v[0] + m[7] * v[1] + m[8] * v[2],
  ];
}

/**
 * Simulate how a dichromat sees a packed sRGB colour.
 *
 * The missing cone response is replaced by the Viénot 1999 projection onto the plane the remaining
 * two cones span: protanopes lose L, deuteranopes M, tritanopes S.
 */
export function simulate(hex: number, model: VisionModel): number {
  if (model === "normal") return hex;
  const lin = unpack(hex).map(toLinear) as [number, number, number];
  const [l, m, s] = mul3(RGB_TO_LMS, lin);
  let lms: [number, number, number];
  switch (model) {
    case "protanopia": lms = [2.02344 * m - 2.52581 * s, m, s]; break;
    case "deuteranopia": lms = [l, 0.494207 * l + 1.24827 * s, s]; break;
    default: lms = [l, m, -0.395913 * l + 0.801109 * m]; break;
  }
  const back = mul3(LMS_TO_RGB, lms);
  return pack([toSrgb(back[0]), toSrgb(back[1]), toSrgb(back[2])]);
}

function toLab(hex: number): [number, number, number] {
  const [r, g, b] = unpack(hex).map(toLinear) as [number, number, number];
  // sRGB → XYZ (D65), then XYZ → CIE L*a*b* against the D65 white point.
  const x = (0.4124564 * r + 0.3575761 * g + 0.1804375 * b) / 0.95047;
  const y = 0.2126729 * r + 0.7151522 * g + 0.0721750 * b;
  const z = (0.0193339 * r + 0.1191920 * g + 0.9503041 * b) / 1.08883;
  const f = (t: number): number => (t > 216 / 24389 ? Math.cbrt(t) : (841 / 108) * t + 4 / 29);
  const fx = f(x);
  const fy = f(y);
  const fz = f(z);
  return [116 * fy - 16, 500 * (fx - fy), 200 * (fy - fz)];
}

/** CIEDE2000 perceptual distance between two packed sRGB colours. */
export function deltaE00(hexA: number, hexB: number): number {
  const [l1, a1, b1] = toLab(hexA);
  const [l2, a2, b2] = toLab(hexB);
  const RAD = Math.PI / 180;
  const c1 = Math.hypot(a1, b1);
  const c2 = Math.hypot(a2, b2);
  const cBar = (c1 + c2) / 2;
  const cBar7 = Math.pow(cBar, 7);
  const g = 0.5 * (1 - Math.sqrt(cBar7 / (cBar7 + Math.pow(25, 7))));
  const a1p = a1 * (1 + g);
  const a2p = a2 * (1 + g);
  const c1p = Math.hypot(a1p, b1);
  const c2p = Math.hypot(a2p, b2);
  const h1p = c1p === 0 ? 0 : ((Math.atan2(b1, a1p) / RAD) + 360) % 360;
  const h2p = c2p === 0 ? 0 : ((Math.atan2(b2, a2p) / RAD) + 360) % 360;
  const dLp = l2 - l1;
  const dCp = c2p - c1p;
  let dhp = 0;
  if (c1p * c2p !== 0) {
    dhp = h2p - h1p;
    if (dhp > 180) dhp -= 360;
    else if (dhp < -180) dhp += 360;
  }
  const dHp = 2 * Math.sqrt(c1p * c2p) * Math.sin((dhp * RAD) / 2);
  const lBarP = (l1 + l2) / 2;
  const cBarP = (c1p + c2p) / 2;
  let hBarP = h1p + h2p;
  if (c1p * c2p !== 0) {
    if (Math.abs(h1p - h2p) > 180) hBarP = h1p + h2p + (h1p + h2p < 360 ? 360 : -360);
    hBarP /= 2;
  } else {
    hBarP = h1p + h2p;
  }
  const t = 1
    - 0.17 * Math.cos((hBarP - 30) * RAD)
    + 0.24 * Math.cos(2 * hBarP * RAD)
    + 0.32 * Math.cos((3 * hBarP + 6) * RAD)
    - 0.20 * Math.cos((4 * hBarP - 63) * RAD);
  const dTheta = 30 * Math.exp(-Math.pow((hBarP - 275) / 25, 2));
  const cBarP7 = Math.pow(cBarP, 7);
  const rc = 2 * Math.sqrt(cBarP7 / (cBarP7 + Math.pow(25, 7)));
  const sl = 1 + (0.015 * Math.pow(lBarP - 50, 2)) / Math.sqrt(20 + Math.pow(lBarP - 50, 2));
  const sc = 1 + 0.045 * cBarP;
  const sh = 1 + 0.015 * cBarP * t;
  const rt = -Math.sin(2 * dTheta * RAD) * rc;
  return Math.sqrt(
    Math.pow(dLp / sl, 2)
    + Math.pow(dCp / sc, 2)
    + Math.pow(dHp / sh, 2)
    + rt * (dCp / sc) * (dHp / sh),
  );
}

/** Perceptual distance as a dichromat of `model` would see it. */
export function deltaE00As(hexA: number, hexB: number, model: VisionModel): number {
  return deltaE00(simulate(hexA, model), simulate(hexB, model));
}

/** The worst (smallest) pairwise distance in a palette, under one vision model. */
export function worstPair(
  entries: readonly { readonly key: string; readonly color: number }[],
  model: VisionModel,
): { a: string; b: string; deltaE: number } {
  let out = { a: "", b: "", deltaE: Infinity };
  for (let i = 0; i < entries.length; i++) {
    for (let j = i + 1; j < entries.length; j++) {
      const d = deltaE00As(entries[i].color, entries[j].color, model);
      if (d < out.deltaE) out = { a: entries[i].key, b: entries[j].key, deltaE: d };
    }
  }
  return out;
}

/** The Okabe–Ito qualitative palette (Okabe & Ito 2008), the set `theme.ts` claims to use. */
export const OKABE_ITO_SET: Readonly<Record<string, number>> = {
  black: 0x000000,
  orange: 0xe69f00,
  skyBlue: 0x56b4e9,
  bluishGreen: 0x009e73,
  yellow: 0xf0e442,
  blue: 0x0072b2,
  vermillion: 0xd55e00,
  reddishPurple: 0xcc79a7,
};

/**
 * True when `hex` sits on the same chromaticity ray as `base` — i.e. it is `base` scaled in linear
 * RGB, which darkens or brightens a colour while keeping its hue and saturation exactly. That is
 * what "a darkened Okabe–Ito blue" has to mean if the claim is to be checkable.
 */
export function sameChromaticity(hex: number, base: number, tolerance = 0.012): boolean {
  const a = unpack(hex).map(toLinear) as [number, number, number];
  const b = unpack(base).map(toLinear) as [number, number, number];
  const sumA = a[0] + a[1] + a[2];
  const sumB = b[0] + b[1] + b[2];
  if (sumA <= 1e-6 || sumB <= 1e-6) return false;
  for (let i = 0; i < 3; i++) {
    if (Math.abs(a[i] / sumA - b[i] / sumB) > tolerance) return false;
  }
  return true;
}

/** Chroma in CIE L*a*b* — how far from neutral grey a colour is. */
export function labChroma(hex: number): number {
  const [, a, b] = toLab(hex);
  return Math.hypot(a, b);
}

/**
 * Chroma of the least chromatic Okabe–Ito hue (sky blue, ~36.8). Anything at less than half of it
 * cannot be read as a categorical hue at all, which is what "a neutral" has to mean for this test
 * to be worth anything.
 */
export const NEUTRAL_CHROMA_MAX = (() => {
  let min = Infinity;
  for (const [, value] of Object.entries(OKABE_ITO_SET)) {
    if (value === 0) continue;
    min = Math.min(min, labChroma(value));
  }
  return min / 2;
})();

/**
 * How a colour relates to the Okabe–Ito set: the exact name, `"<name> (scaled)"` for a darkened or
 * brightened version of one, `"neutral"` for something too low in chroma to read as a hue, or null.
 */
export function okabeItoRelation(hex: number): string | null {
  const exact = okabeItoName(hex);
  if (exact) return exact;
  for (const [name, value] of Object.entries(OKABE_ITO_SET)) {
    if (value !== 0 && sameChromaticity(hex, value)) return `${name} (scaled)`;
  }
  return labChroma(hex) < NEUTRAL_CHROMA_MAX ? "neutral" : null;
}

/** The Okabe–Ito name of a colour, or null when it is not in the set. */
export function okabeItoName(hex: number): string | null {
  for (const [name, value] of Object.entries(OKABE_ITO_SET)) if (value === hex) return name;
  return null;
}
