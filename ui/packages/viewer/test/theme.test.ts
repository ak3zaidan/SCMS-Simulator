/**
 * The palette tests (findings Q5 and Q13).
 *
 * Q5: `theme.ts`'s header claims the actor-state colours are Okabe–Ito and 09-ui §10 requires a
 * colour-blind-safe categorical palette. Both are measurable, so they are measured: every entry is
 * checked against the real Okabe–Ito set (exactly, or as a luminance-scaled version of one, or as a
 * deliberate neutral), every pair is checked under three dichromat simulations with CIEDE2000, and
 * every colour is checked against WCAG 2.1 SC 1.4.11's 3:1 non-text contrast floor on its own
 * background.
 *
 * Q13: what the legend says and what the scene draws must be the same thing. The legend is read
 * from `ActorRenderer.legend()` and compared against the bytes the renderer actually wrote into
 * `instanceColor`, so a divergence fails here rather than in a screenshot.
 */

import { describe, expect, it } from "vitest";
import { Color, PerspectiveCamera } from "three";
import { ActorState } from "@vwp/protocol";
import { ACTOR_STATE_COLOR_KEYS, ActorRenderer, actorColorKey } from "../src/actors.js";
import { DARK_THEME, LIGHT_THEME, themeByName, type ViewerTheme } from "../src/theme.js";
import {
  NEUTRAL_CHROMA_MAX, OKABE_ITO_SET, VISION_MODELS, contrastRatio, css, deltaE00As, labChroma,
  okabeItoRelation, sameChromaticity,
} from "./support/cvd.js";

/** ΔE00 below this is not a reliable categorical distinction (the usual bar for encodings). */
const MIN_STATE_DELTA_E = 15;
/**
 * `selected` is a transient highlight that also carries its own marker shape (a ring), so it is
 * held to a lower bar than the four categorical states — but not to none.
 */
const MIN_SELECTED_DELTA_E = 9;
/** WCAG 2.1 SC 1.4.11, non-text contrast. */
const MIN_CONTRAST = 3;

const THEMES: readonly ViewerTheme[] = [DARK_THEME, LIGHT_THEME];

function entriesOf(theme: ViewerTheme): { key: string; color: number }[] {
  return ACTOR_STATE_COLOR_KEYS.map((key) => ({ key, color: theme.actorState[key] }));
}

describe("actor-state palette (Q5)", () => {
  it("is drawn from the real Okabe–Ito set, or is a documented derivative of it", () => {
    for (const theme of THEMES) {
      for (const { key, color } of entriesOf(theme)) {
        const relation = okabeItoRelation(color);
        expect(relation, `${theme.name}.${key} = ${css(color)} is in no palette`).not.toBeNull();
        if (key === "benign" || key === "selected") {
          // These two are deliberately neutral: benign means "nothing to report" and selected is a
          // highlight, so neither should compete with a categorical hue.
          expect(labChroma(color), `${theme.name}.${key} should be near-neutral`)
            .toBeLessThan(NEUTRAL_CHROMA_MAX);
        } else {
          expect(relation, `${theme.name}.${key}`).not.toBe("neutral");
        }
      }
    }
    // The specific claim the review contested: the light theme's `reported` was 0xb08900, a gold
    // that is in no palette at all.
    expect(LIGHT_THEME.actorState.reported).toBe(OKABE_ITO_SET.blue);
    expect(okabeItoRelation(LIGHT_THEME.actorState.reported)).toBe("blue");
    expect(DARK_THEME.actorState.reported).toBe(OKABE_ITO_SET.yellow);
    expect(DARK_THEME.actorState.attacker).toBe(OKABE_ITO_SET.vermillion);
    expect(LIGHT_THEME.actorState.attacker).toBe(OKABE_ITO_SET.vermillion);
    // Both `revoked` values are the Okabe–Ito reddish purple with its luminance scaled down: same
    // hue, same saturation, enough lightness separation to survive protanopia.
    for (const theme of THEMES) {
      expect(
        sameChromaticity(theme.actorState.revoked, OKABE_ITO_SET.reddishPurple),
        `${theme.name}.revoked is not on the reddish-purple ray`,
      ).toBe(true);
    }
  });

  it("clears 3:1 non-text contrast against its own background", () => {
    const rows: string[] = [];
    for (const theme of THEMES) {
      for (const { key, color } of entriesOf(theme)) {
        const ratio = contrastRatio(color, theme.background);
        rows.push(`${theme.name}.${key.padEnd(9)} ${css(color)} ${ratio.toFixed(2)}:1`);
        expect(ratio, `${theme.name}.${key} contrast`).toBeGreaterThanOrEqual(MIN_CONTRAST);
      }
    }
    // eslint-disable-next-line no-console
    console.log(`\ncontrast against the theme background (WCAG 2.1 SC 1.4.11, 3:1)\n  ${rows.join("\n  ")}`);
  });

  it("keeps every pair apart under all three dichromacies", () => {
    const rows: string[] = [];
    for (const theme of THEMES) {
      const entries = entriesOf(theme);
      for (const model of VISION_MODELS) {
        let worstState = { pair: "", d: Infinity };
        let worstSelected = { pair: "", d: Infinity };
        for (let i = 0; i < entries.length; i++) {
          for (let j = i + 1; j < entries.length; j++) {
            const a = entries[i];
            const b = entries[j];
            const d = deltaE00As(a.color, b.color, model);
            const pair = `${a.key}/${b.key}`;
            if (a.key === "selected" || b.key === "selected") {
              if (d < worstSelected.d) worstSelected = { pair, d };
            } else if (d < worstState.d) {
              worstState = { pair, d };
            }
          }
        }
        rows.push(
          `${theme.name}/${model.padEnd(13)} states ≥ ${worstState.d.toFixed(1)} (${worstState.pair}), `
          + `selection ≥ ${worstSelected.d.toFixed(1)} (${worstSelected.pair})`,
        );
        expect(worstState.d, `${theme.name}/${model} ${worstState.pair}`)
          .toBeGreaterThanOrEqual(MIN_STATE_DELTA_E);
        expect(worstSelected.d, `${theme.name}/${model} ${worstSelected.pair}`)
          .toBeGreaterThanOrEqual(MIN_SELECTED_DELTA_E);
      }
    }
    // eslint-disable-next-line no-console
    console.log(`\nminimum pairwise CIEDE2000 (Viénot dichromat simulation)\n  ${rows.join("\n  ")}`);
  });

  it("would fail on the palette the review measured", () => {
    // The instrument itself, checked against the review's own numbers: the old light-theme gold
    // collapses onto vermillion under deuteranopia and fails the contrast floor.
    const oldGold = 0xb08900;
    expect(okabeItoRelation(oldGold)).toBeNull();
    expect(contrastRatio(oldGold, LIGHT_THEME.background)).toBeLessThan(MIN_CONTRAST);
    expect(contrastRatio(oldGold, LIGHT_THEME.background)).toBeCloseTo(2.77, 1);
    expect(deltaE00As(oldGold, 0xd55e00, "deuteranopia")).toBeLessThan(5);
    // And the old mid-lightness purple collapses onto the dark theme's benign grey.
    expect(deltaE00As(0xcc79a7, 0x9fb4c7, "deuteranopia")).toBeLessThan(MIN_STATE_DELTA_E);
    expect(contrastRatio(0xcc79a7, LIGHT_THEME.background)).toBeCloseTo(2.60, 1);
  });

  it("carries the redundant shape channel without being switched on (Q14)", async () => {
    // 09-ui §10: colour-blind-safe palette *with shape redundancy*. A shape channel that has to be
    // enabled first is not redundancy, so the three non-ground-truth marker channels start on.
    const { OverlayManager } = await import("../src/overlays.js");
    const overlays = new OverlayManager({ theme: DARK_THEME });
    for (const name of ["reported", "revoked", "detections"] as const) {
      expect(overlays.isEnabled(name), name).toBe(true);
    }
    expect(overlays.markers.isChannelEnabled("reported")).toBe(true);
    expect(overlays.markers.isChannelEnabled("revoked")).toBe(true);
    expect(overlays.markers.isChannelEnabled("detection")).toBe(true);
    // The ground-truth channel is the one the blind-evaluation lock exists for; it stays off.
    expect(overlays.isEnabled("attackers_gt")).toBe(false);
    expect(overlays.markers.isChannelEnabled("attacker")).toBe(false);
    overlays.dispose();
  });

  it("resolves themes by name", () => {
    expect(themeByName("light")).toBe(LIGHT_THEME);
    expect(themeByName("dark")).toBe(DARK_THEME);
    expect(themeByName("nonsense")).toBe(DARK_THEME);
  });
});

describe("legend and scene agree (Q13)", () => {
  const camera = new PerspectiveCamera(60, 1.6, 0.1, 5000);
  camera.up.set(0, 0, 1);
  camera.position.set(0, -60, 40);
  camera.lookAt(0, 0, 0);
  camera.updateMatrixWorld();
  camera.updateProjectionMatrix();

  /** Four actors in a row, one per state bucket, plus one selected. */
  function context(actorRenderer: ActorRenderer): Parameters<ActorRenderer["update"]>[0] {
    const states = [
      0,
      ActorState.REPORTED,
      ActorState.REVOKED,
      ActorState.ATTACKER,
      0,
    ];
    const n = states.length;
    const position = new Float32Array(n * 3);
    const heading = new Float32Array(n);
    const classIdx = new Uint8Array(n);
    const state = new Uint8Array(states);
    const occupied = new Uint8Array(n).fill(1);
    const actorId = new Uint32Array(n);
    for (let i = 0; i < n; i++) {
      position[i * 3] = (i - 2) * 6;
      actorId[i] = i + 1;
      classIdx[i] = i % 3;
    }
    actorRenderer.selectedActorId = 5; // the last actor
    return { position, heading, classIdx, state, occupied, actorId, count: n, camera };
  }

  function drawnColors(r: ActorRenderer): Map<number, Color> {
    // slot → the colour actually written into that instance's `instanceColor`.
    const out = new Map<number, Color>();
    for (let c = 0; c < r.classes.length; c++) {
      for (const lod of [0, 1, 2] as const) {
        const bucket = r.bucketAt(c, lod);
        if (!bucket || bucket.count === 0) continue;
        const colors = bucket.mesh.instanceColor?.array as Float32Array | undefined;
        if (!colors) continue;
        // `bucketAt` does not expose the slot list, so re-derive it from the instance matrices'
        // translation, which is what the write loop put there.
        const m = bucket.mesh.instanceMatrix.array as Float32Array;
        for (let i = 0; i < bucket.count; i++) {
          const x = m[i * 16 + 12];
          const slot = Math.round(x / 6) + 2;
          out.set(slot, new Color(colors[i * 3], colors[i * 3 + 1], colors[i * 3 + 2]));
        }
      }
    }
    return out;
  }

  for (const theme of THEMES) {
    it(`draws exactly the legend's colours (${theme.name})`, () => {
      const renderer = new ActorRenderer({ classes: [], theme });
      const ctx = context(renderer);
      renderer.update(ctx);

      const legend = renderer.legend();
      const byKey = new Map(legend.map((e) => [e.key, e.color]));
      // Every state the renderer can paint has a legend row, and no row is unreachable.
      expect([...byKey.keys()].sort()).toEqual(
        ["attacker", "benign", "reported", "revoked", "selected"],
      );

      const painted = drawnColors(renderer);
      expect(painted.size).toBe(5);
      const expectedKeys = ["benign", "reported", "revoked", "attacker", "selected"];
      for (let slot = 0; slot < 5; slot++) {
        const key = actorColorKey(ctx.state[slot], ctx.actorId[slot] === 5, true);
        expect(key).toBe(expectedKeys[slot]);
        const want = new Color().setHex(byKey.get(key) as number);
        const got = painted.get(slot) as Color;
        expect(got, `slot ${slot} (${key})`).toBeDefined();
        // This is the Q13 assertion: the bytes on the GPU are the legend's colour, to the last bit.
        expect(got.r).toBeCloseTo(want.r, 5);
        expect(got.g).toBeCloseTo(want.g, 5);
        expect(got.b).toBeCloseTo(want.b, 5);
      }
      renderer.dispose();
    });
  }

  it("reports class colours instead of a benign row when it draws them", () => {
    // The old behaviour, now opt-in: benign actors take their class colour. The legend has to say
    // so — the failure mode Q13 describes is a legend row nothing on screen matches.
    const renderer = new ActorRenderer({
      classes: [], theme: DARK_THEME, colorBenignByClass: true,
    });
    const ctx = context(renderer);
    renderer.update(ctx);
    const legend = renderer.legend();
    expect(legend.some((e) => e.key === "benign")).toBe(false);
    const classRows = legend.filter((e) => e.kind === "class");
    expect(classRows.length).toBe(renderer.classes.length);

    const painted = drawnColors(renderer);
    for (const slot of [0, 4]) {
      const isSelected = ctx.actorId[slot] === 5;
      const cls = renderer.classes[ctx.classIdx[slot]];
      const row = isSelected
        ? legend.find((e) => e.key === "selected")
        : classRows.find((e) => e.classIndex === cls.index);
      const want = new Color().setHex(row?.color as number);
      const got = painted.get(slot) as Color;
      expect(got.r).toBeCloseTo(want.r, 5);
      expect(got.g).toBeCloseTo(want.g, 5);
      expect(got.b).toBeCloseTo(want.b, 5);
    }
    renderer.dispose();
  });

  it("drops the attacker row when ground truth is locked off", () => {
    const renderer = new ActorRenderer({ classes: [], theme: DARK_THEME });
    renderer.showGroundTruth = false;
    expect(renderer.legend().some((e) => e.key === "attacker")).toBe(false);
    const ctx = context(renderer);
    renderer.update(ctx);
    const painted = drawnColors(renderer);
    // The attacker (slot 3, GT-only) now draws as benign, and the legend agrees.
    const want = new Color().setHex(DARK_THEME.actorState.benign);
    const got = painted.get(3) as Color;
    expect(got.r).toBeCloseTo(want.r, 5);
    expect(got.g).toBeCloseTo(want.g, 5);
    expect(got.b).toBeCloseTo(want.b, 5);
    renderer.dispose();
  });
});
