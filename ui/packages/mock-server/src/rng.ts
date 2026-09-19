/** A small deterministic PRNG, so a given seed always produces the same world and the same run. */

/** Mulberry32 — 32-bit state, good enough for a fixture and identical on every platform. */
export function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** Uniform integer in `[lo, hi)`. */
export const randInt = (rand: () => number, lo: number, hi: number): number => lo + Math.floor(rand() * (hi - lo));
/** Uniform float in `[lo, hi)`. */
export const randRange = (rand: () => number, lo: number, hi: number): number => lo + rand() * (hi - lo);
/** Pick one element. */
export const pick = <T>(rand: () => number, items: readonly T[]): T => items[randInt(rand, 0, items.length)];
