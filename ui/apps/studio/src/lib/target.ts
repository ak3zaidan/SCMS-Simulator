/**
 * Which engine the Studio talks to.
 *
 * Until now the answer was "whatever `window.location.origin` proxies to", which in development is
 * the mock (`@vwp/mock-server`). `crates/v2xw-server` was built to be a drop-in replacement for it
 * — same four routes (`/vwp/v1`, `/world/{hash}.vwb`, `/rpc`, `/rpc/schema`), same `/healthz`
 * shape, the same 32 methods of vwp-v1 §6.15 — so the choice is a base URL and nothing else.
 *
 * This module resolves that base URL, in this order:
 *
 *  1. an explicit override — `?engine=<url>` in the page URL, which also persists it, or the
 *     previously persisted value;
 *  2. the page's own origin, which is what the production deployment and the Vite dev proxy both
 *     serve (09-ui §9: "the engine serves the built Studio"). In development that proxy points at
 *     `VWP_ENGINE`, which defaults to `http://127.0.0.1:8787` — the real server's own default bind
 *     — so starting `v2xw-server` there is all it takes to be talking to the real engine;
 *  3. the real server's default bind, probed directly, for a page served from somewhere else;
 *  4. the mock's default bind, which is the same port, so this candidate only differs once one of
 *     the two is moved.
 *
 * The first reachable candidate wins, except that among the *discovery* candidates (3 and 4) a real
 * engine beats a fixture one — which is what "default to the real server when one is reachable"
 * means for the ports this module goes looking at. It deliberately does not second-guess candidate
 * 2: `pnpm test:e2e` keeps the fixture by pointing the proxy at it, and that is a decision, not a
 * hint. The topbar chip always reports which flavour answered.
 *
 * Nothing in here touches the DOM or the real `fetch`: the probe takes both as arguments, so
 * `test/target.test.ts` drives every branch in plain Node.
 */

/** Which implementation answered `/healthz`. */
export type EngineFlavour = "real" | "mock" | "unknown";

/** §1.1 — the `/healthz` body both implementations return. */
export interface HealthzBody {
  readonly ok?: boolean;
  /** `v2xw-server 0.1.0` for the Rust engine, `vwp-mock-server 0.1.0` for the fixture. */
  readonly engine?: string;
  readonly runs?: readonly string[];
}

/** What one probe of one candidate learned. */
export interface EngineProbe {
  /** Absolute origin with no trailing slash, or `""` for the page's own origin. */
  readonly baseUrl: string;
  readonly reachable: boolean;
  readonly flavour: EngineFlavour;
  /** The `engine` string `/healthz` reported, or the failure text. */
  readonly engine: string;
  readonly runs: readonly string[];
  /** HTTP status, or 0 when the request never completed. */
  readonly status: number;
}

/** A candidate to probe, with the label the UI shows for it. */
export interface EngineCandidate {
  readonly baseUrl: string;
  readonly label: string;
  /** True for a candidate the user named explicitly; those skip the real-over-mock preference. */
  readonly pinned: boolean;
  /**
   * True for a candidate that is a *configuration*, not a discovery — the page's own origin.
   *
   * Whatever answers there was put there on purpose: the deployed build is served by the engine
   * (09-ui §9), and in development `vite.config.ts` proxies `/vwp/v1`, `/rpc` and `/world` onto
   * `VWP_ENGINE`, which is how `playwright.config.ts` keeps the end-to-end suite on the fixture.
   * Overriding that by scanning localhost ports would make the proxy setting a suggestion, and
   * would fetch a cross-origin URL the page has no business asking for. So a reachable
   * authoritative candidate wins outright, whatever flavour it turns out to be — and the chip in
   * the topbar reports the flavour, which is the part that has to be honest.
   */
  readonly authoritative?: boolean;
}

/** The subset of `Storage` this module uses, so a test can pass a plain object. */
export interface StorageLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

/** Where the pinned target is remembered. */
export const ENGINE_STORAGE_KEY = "vwp.studio.engine";

/** `ServerOptions::default()` in `crates/v2xw-server/src/lib.rs`. */
export const REAL_SERVER_DEFAULT = "http://127.0.0.1:8787";

/** `MockEngineOptions.port` in `ui/packages/mock-server/src/server.ts`. */
export const MOCK_SERVER_DEFAULT = "http://127.0.0.1:8787";

/** How long a probe waits before calling a candidate unreachable. */
export const PROBE_TIMEOUT_MS = 1500;

/**
 * Normalise a base URL: trim, drop trailing slashes, and turn a `ws(s)://` origin into the
 * `http(s)://` one, because `/healthz` and `/world` are HTTP and {@link VwpClient} does the reverse
 * conversion itself (`VwpClient.endpointUrl`).
 *
 * The empty string is the page's own origin and is returned unchanged.
 */
export function normaliseBase(url: string): string {
  const trimmed = url.trim().replace(/\/+$/, "");
  if (trimmed === "") return "";
  if (trimmed.startsWith("ws://")) return `http://${trimmed.slice(5)}`;
  if (trimmed.startsWith("wss://")) return `https://${trimmed.slice(6)}`;
  return trimmed;
}

/**
 * Which implementation a `/healthz` `engine` string names.
 *
 * Both start with the crate or package name and a version, which is the only part that is stable
 * enough to match on: `v2xw-server 0.1.0` against `vwp-mock-server 0.1.0`. Anything else is
 * `unknown` and is still usable — the protocol is the contract, not the banner.
 */
export function engineFlavour(engine: string): EngineFlavour {
  const name = engine.trim().toLowerCase();
  if (name.startsWith("v2xw-server")) return "real";
  if (name.startsWith("vwp-mock-server")) return "mock";
  return "unknown";
}

/** A short, human label for a probe result. */
export function describeProbe(probe: EngineProbe): string {
  const where = probe.baseUrl === "" ? "this origin" : probe.baseUrl;
  if (!probe.reachable) return `${where}: ${probe.engine}`;
  return `${where}: ${probe.engine} (${probe.flavour})`;
}

/**
 * Ask one candidate what it is.
 *
 * A non-200, a body that is not the `/healthz` object, a network error and a timeout are all the
 * same answer — not reachable — and the reason is kept in `engine` so the UI can show it rather
 * than just failing to connect later.
 */
export async function probeEngine(
  baseUrl: string,
  fetchImpl: typeof fetch,
  timeoutMs = PROBE_TIMEOUT_MS,
): Promise<EngineProbe> {
  const base = normaliseBase(baseUrl);
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    const res = await fetchImpl(`${base}/healthz`, { signal: controller.signal, cache: "no-store" });
    if (!res.ok) {
      return { baseUrl: base, reachable: false, flavour: "unknown", engine: `HTTP ${res.status}`, runs: [], status: res.status };
    }
    const body = (await res.json()) as HealthzBody | null;
    const engine = typeof body?.engine === "string" ? body.engine : "";
    if (body?.ok !== true || engine === "") {
      return { baseUrl: base, reachable: false, flavour: "unknown", engine: "not a VWP /healthz body", runs: [], status: res.status };
    }
    const runs = Array.isArray(body.runs) ? body.runs.filter((r): r is string => typeof r === "string") : [];
    return { baseUrl: base, reachable: true, flavour: engineFlavour(engine), engine, runs, status: res.status };
  } catch (err) {
    const why = err instanceof Error ? (err.name === "AbortError" ? `no answer in ${timeoutMs} ms` : err.message) : String(err);
    return { baseUrl: base, reachable: false, flavour: "unknown", engine: why, runs: [], status: 0 };
  } finally {
    clearTimeout(timer);
  }
}

/** The pinned override, from the page URL first and then from storage. `null` when there is none. */
export function readOverride(search: string, storage: StorageLike | null): string | null {
  let fromQuery: string | null = null;
  try {
    const params = new URLSearchParams(search.startsWith("?") ? search.slice(1) : search);
    const engine = params.get("engine");
    if (engine !== null && engine.trim() !== "") fromQuery = normaliseBase(engine);
    // `?engine=origin` is how a reviewer un-pins without opening dev tools.
    if (engine !== null && engine.trim().toLowerCase() === "origin") fromQuery = "";
  } catch {
    fromQuery = null;
  }
  if (fromQuery !== null) {
    // A query override is sticky, so a reload keeps the same engine.
    try {
      storage?.setItem(ENGINE_STORAGE_KEY, fromQuery);
    } catch {
      /* private browsing, or storage disabled; the override still applies to this page */
    }
    return fromQuery;
  }
  try {
    const stored = storage?.getItem(ENGINE_STORAGE_KEY) ?? null;
    return stored === null ? null : normaliseBase(stored);
  } catch {
    return null;
  }
}

/** Forget the pinned override, so the next resolve goes back to preferring a real engine. */
export function clearOverride(storage: StorageLike | null): void {
  try {
    storage?.removeItem(ENGINE_STORAGE_KEY);
  } catch {
    /* nothing to do */
  }
}

/** Remember `baseUrl` as the pinned override. */
export function writeOverride(baseUrl: string, storage: StorageLike | null): void {
  try {
    storage?.setItem(ENGINE_STORAGE_KEY, normaliseBase(baseUrl));
  } catch {
    /* nothing to do */
  }
}

/**
 * The candidates to probe, in order, de-duplicated by base URL.
 *
 * An override collapses the list to one pinned entry: the user named an engine, and silently
 * probing something else would make the pin a suggestion.
 */
export function candidateTargets(override: string | null): readonly EngineCandidate[] {
  if (override !== null) {
    return [{ baseUrl: override, label: override === "" ? "this origin (pinned)" : `${override} (pinned)`, pinned: true }];
  }
  const out: EngineCandidate[] = [
    { baseUrl: "", label: "this origin", pinned: false, authoritative: true },
    { baseUrl: normaliseBase(REAL_SERVER_DEFAULT), label: "v2xw-server default", pinned: false },
    { baseUrl: normaliseBase(MOCK_SERVER_DEFAULT), label: "mock default", pinned: false },
  ];
  const seen = new Set<string>();
  const unique: EngineCandidate[] = [];
  for (const candidate of out) {
    if (seen.has(candidate.baseUrl)) continue;
    seen.add(candidate.baseUrl);
    unique.push(candidate);
  }
  return unique;
}

/** What {@link resolveEngineTarget} decided. */
export interface EngineTarget {
  /** The base URL to hand {@link VwpClient}; `""` means the page's own origin. */
  readonly baseUrl: string;
  readonly probe: EngineProbe | null;
  /** Every candidate that was probed, in order, so the UI can say what it tried. */
  readonly tried: readonly EngineProbe[];
  readonly pinned: boolean;
}

/**
 * Probe the candidates and pick one.
 *
 * Probes run in sequence, not in parallel: three concurrent requests to a possibly-absent localhost
 * port produce three console errors for nothing, and in the common case the first candidate answers
 * and the rest are never asked.
 *
 * A reachable candidate wins immediately when it is **pinned** (the user named it), when it is
 * **authoritative** (the page's own origin — see {@link EngineCandidate.authoritative}), or when it
 * turns out to be a **real** engine. A reachable mock among the discovery candidates is kept as a
 * fallback and returned only if no real engine answered, which is the "default to the real server
 * when one is reachable" rule applied where it belongs: to the ports this module goes looking at,
 * not to the one the deployment chose.
 *
 * With nothing reachable the first candidate is returned anyway, so the connect attempt — and its
 * error message — happens against the target the user would expect.
 */
export async function resolveEngineTarget(
  candidates: readonly EngineCandidate[],
  fetchImpl: typeof fetch,
  timeoutMs = PROBE_TIMEOUT_MS,
): Promise<EngineTarget> {
  const tried: EngineProbe[] = [];
  let fallback: EngineProbe | null = null;
  for (const candidate of candidates) {
    const probe = await probeEngine(candidate.baseUrl, fetchImpl, timeoutMs);
    tried.push(probe);
    if (!probe.reachable) continue;
    if (candidate.pinned || candidate.authoritative === true || probe.flavour === "real") {
      return { baseUrl: probe.baseUrl, probe, tried, pinned: candidate.pinned };
    }
    fallback ??= probe;
  }
  if (fallback !== null) {
    return { baseUrl: fallback.baseUrl, probe: fallback, tried, pinned: false };
  }
  const first = candidates[0];
  return {
    baseUrl: first === undefined ? "" : first.baseUrl,
    probe: tried[0] ?? null,
    tried,
    pinned: first?.pinned ?? false,
  };
}

/**
 * Resolve a URL that arrived from the engine against the engine's own base.
 *
 * `Hello.world_ref.str_url` is `/world/{hash}.vwb` — root-relative, because §3.1.6 assumes the
 * engine and the page are the same origin. They are the same origin behind the Vite proxy and in
 * the deployed build, but not when the Studio is pointed at `http://127.0.0.1:8787` from a page
 * served on :5173, and a bare `fetch("/world/…")` then asks the *page's* origin for a world it
 * does not have. Joining against the base URL is what makes the pinned-target case work at all.
 *
 * An absolute URL the engine supplied is returned unchanged, which is what §3.1.6 allows for a
 * world served from object storage.
 */
export function resolveEngineUrl(baseUrl: string, url: string): string {
  if (/^[a-z][a-z0-9+.-]*:/i.test(url)) return url;
  const base = normaliseBase(baseUrl);
  if (base === "") return url;
  return url.startsWith("/") ? `${base}${url}` : `${base}/${url}`;
}

/**
 * Whether fetching `baseUrl`'s world payload from this page will be blocked.
 *
 * Both servers set `cross-origin-resource-policy: same-origin` on every response (§1.1), so a
 * cross-origin world fetch fails in the browser however permissive CORS is. The Studio can still
 * *stream* from such an engine — a WebSocket is not subject to CORP — so this is a warning the UI
 * shows rather than a refusal, and the fix is either the dev proxy or `Hello.world_ref.mode = 1`,
 * which delivers the world as §3.9 chunks on the socket instead.
 */
export function worldFetchBlocked(baseUrl: string, pageOrigin: string): boolean {
  const base = normaliseBase(baseUrl);
  if (base === "") return false;
  try {
    return new URL(base).origin !== new URL(pageOrigin).origin;
  } catch {
    return false;
  }
}
