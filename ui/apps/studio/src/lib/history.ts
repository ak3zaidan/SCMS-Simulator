/**
 * Fixed-capacity time series for the sparkline row and the plots strip.
 *
 * uPlot wants `[xs, ...ys]` as plain arrays of the *same* length, so the buffers are kept as
 * `Float64Array` rings and materialised into the exact arrays uPlot asks for. Nothing here
 * allocates per sample: telemetry lands at `telemetry_period_ns` (§3.1.1, 1 s by default) and the
 * strip is redrawn on the same beat.
 *
 * Both stores are rings with a `start`/`count` pair, so an eviction is one index increment. Neither
 * uses `Array.prototype.shift()`, which would make every push past capacity O(n) — see the timing
 * test in `test/history.test.ts`.
 */

/** A uPlot-ready data tuple: x first, then one array per series. */
export type UplotData = [number[], ...(number | null)[][]];

/** One aligned multi-series ring: one x column, `keys.length` y columns. */
export class SeriesRing {
  readonly keys: readonly string[];
  readonly capacity: number;

  #x: Float64Array;
  #y: Float64Array[];
  #valid: Uint8Array[];
  #start = 0;
  #count = 0;

  constructor(keys: readonly string[], capacity = 600) {
    this.keys = keys;
    this.capacity = Math.max(2, capacity);
    this.#x = new Float64Array(this.capacity);
    this.#y = keys.map(() => new Float64Array(this.capacity));
    this.#valid = keys.map(() => new Uint8Array(this.capacity));
  }

  /** Samples currently retained. */
  get count(): number {
    return this.#count;
  }

  /** The newest x, or `null` when empty. */
  get lastX(): number | null {
    if (this.#count === 0) return null;
    return this.#x[(this.#start + this.#count - 1) % this.capacity];
  }

  /** Append one aligned sample. `null` in `values` marks a gap the plot will not draw. */
  push(x: number, values: readonly (number | null)[]): void {
    const i = (this.#start + this.#count) % this.capacity;
    this.#x[i] = x;
    for (let k = 0; k < this.#y.length; k++) {
      const v = values[k];
      const ok = v !== null && v !== undefined && Number.isFinite(v);
      this.#y[k][i] = ok ? (v as number) : 0;
      this.#valid[k][i] = ok ? 1 : 0;
    }
    if (this.#count < this.capacity) this.#count++;
    else this.#start = (this.#start + 1) % this.capacity;
  }

  /** Drop every sample. */
  reset(): void {
    this.#start = 0;
    this.#count = 0;
  }

  /** The newest value of series `k`, or `null`. */
  latest(k: number): number | null {
    if (this.#count === 0) return null;
    const i = (this.#start + this.#count - 1) % this.capacity;
    return this.#valid[k][i] === 1 ? this.#y[k][i] : null;
  }

  /** Materialise into the `[xs, ...ys]` shape uPlot's `setData` takes. */
  toUplot(): UplotData {
    const n = this.#count;
    const xs = new Array<number>(n);
    const ys: (number | null)[][] = this.#y.map(() => new Array<number | null>(n));
    for (let j = 0; j < n; j++) {
      const i = (this.#start + j) % this.capacity;
      xs[j] = this.#x[i];
      for (let k = 0; k < ys.length; k++) ys[k][j] = this.#valid[k][i] === 1 ? this.#y[k][i] : null;
    }
    return [xs, ...ys];
  }

  /** One series as `[xs, ys]`, for a single-series sparkline. */
  toUplotOne(k: number): UplotData {
    const n = this.#count;
    const xs = new Array<number>(n);
    const ys = new Array<number | null>(n);
    for (let j = 0; j < n; j++) {
      const i = (this.#start + j) % this.capacity;
      xs[j] = this.#x[i];
      ys[j] = this.#valid[k][i] === 1 ? this.#y[k][i] : null;
    }
    return [xs, ys];
  }
}

/** One named series: the same `Float64Array` ring `SeriesRing` uses, for a single y column. */
interface MetricSeries {
  readonly xs: Float64Array;
  readonly ys: Float64Array;
  start: number;
  count: number;
}

/**
 * How many distinct metric names are kept resident.
 *
 * `MetricSample.str_metric` (§3.7) is a wire-supplied string id, so the name set is attacker- and
 * scenario-controlled and nothing else prunes it — `reset()` only runs on disconnect. The oldest
 * series is evicted once the ceiling is reached, which bounds both the memory and the cost of
 * `names()`, the list the plots strip rebuilds on every 5 Hz tick.
 */
const DEFAULT_MAX_SERIES = 512;

/** A growable set of named series sampled on a shared clock — the plots strip's metric store. */
export class MetricHistory {
  readonly capacity: number;
  readonly maxSeries: number;

  #series = new Map<string, MetricSeries>();
  #names: readonly string[] = [];
  #version = 0;

  constructor(capacity = 900, maxSeries = DEFAULT_MAX_SERIES) {
    this.capacity = Math.max(2, capacity);
    this.maxSeries = Math.max(1, maxSeries);
  }

  /**
   * Bumped whenever a series appears or is evicted, never when a sample lands.
   *
   * The plots strip watches this instead of comparing `names().length`, which silently missed a
   * renamed metric at a constant count.
   */
  get seriesVersion(): number {
    return this.#version;
  }

  /** Every metric name seen so far, sorted. Cached: the same array until the series set changes. */
  names(): readonly string[] {
    return this.#names;
  }

  /** Record one sample of `name` at simulated time `tSeconds`. O(1), and allocation-free. */
  push(name: string, tSeconds: number, value: number): void {
    let s = this.#series.get(name);
    if (!s) {
      if (this.#series.size >= this.maxSeries) {
        // Map iteration is insertion-ordered, so the first key is the least recently added.
        const oldest = this.#series.keys().next().value;
        if (oldest !== undefined) this.#series.delete(oldest);
      }
      s = { xs: new Float64Array(this.capacity), ys: new Float64Array(this.capacity), start: 0, count: 0 };
      this.#series.set(name, s);
      this.#names = [...this.#series.keys()].sort();
      this.#version++;
    }
    // Metric frames repeat the bin's end time; a repeat overwrites rather than stacking.
    if (s.count > 0) {
      const last = (s.start + s.count - 1) % this.capacity;
      if (s.xs[last] === tSeconds) {
        s.ys[last] = value;
        return;
      }
    }
    const i = (s.start + s.count) % this.capacity;
    s.xs[i] = tSeconds;
    s.ys[i] = value;
    if (s.count < this.capacity) s.count++;
    else s.start = (s.start + 1) % this.capacity;
  }

  /** `[xs, ys]` for one metric, or empty arrays when it has never been seen. */
  get(name: string): UplotData {
    const s = this.#series.get(name);
    if (!s) return [[], []];
    const n = s.count;
    const xs = new Array<number>(n);
    const ys = new Array<number | null>(n);
    for (let j = 0; j < n; j++) {
      const i = (s.start + j) % this.capacity;
      xs[j] = s.xs[i];
      ys[j] = s.ys[i];
    }
    return [xs, ys];
  }

  /** The newest value of `name`, or `null`. */
  latest(name: string): number | null {
    const s = this.#series.get(name);
    if (!s || s.count === 0) return null;
    return s.ys[(s.start + s.count - 1) % this.capacity];
  }

  /** Whether `name` has ever been sampled — a metric one run reports and another does not. */
  has(name: string): boolean {
    return this.#series.has(name);
  }

  /**
   * The value of `name` at or before `tSeconds`, or `null` when the series starts later.
   *
   * This is what the side-by-side difference view reads: "the same simulated time" has to mean the
   * same instant in both runs, and a metric sampled once a second is almost never sampled at the
   * instant a scrub lands on. Taking the last sample at or before *t* is a step interpolation,
   * which is the honest reading of a binned metric (§3.7 samples carry the bin's end time), and
   * deliberately not a linear one: interpolating between two bins would invent a value.
   *
   * Binary search over the ring's logical index: the buffer is written in ascending time and a
   * repeat overwrites in place, so the logical order is sorted.
   */
  at(name: string, tSeconds: number): number | null {
    const s = this.#series.get(name);
    if (!s || s.count === 0 || !Number.isFinite(tSeconds)) return null;
    const xAt = (j: number): number => s.xs[(s.start + j) % this.capacity];
    if (xAt(0) > tSeconds) return null;
    let lo = 0;
    let hi = s.count - 1;
    // Invariant: xAt(lo) <= t. Find the greatest index whose x is <= t.
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (xAt(mid) <= tSeconds) lo = mid;
      else hi = mid - 1;
    }
    return s.ys[(s.start + lo) % this.capacity];
  }

  /** Forget everything (a new run, or a seek). */
  reset(): void {
    if (this.#series.size === 0) return;
    this.#series.clear();
    this.#names = [];
    this.#version++;
  }
}
