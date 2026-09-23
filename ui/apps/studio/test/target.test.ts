/**
 * The engine-target resolver.
 *
 * `crates/v2xw-server` and `ui/packages/mock-server` answer the same routes, and the Studio used to
 * be pinned to whichever one the dev proxy happened to point at. These tests pin the decision this
 * module now makes instead: prefer the real engine, honour an explicit pin, and never silently pick
 * the fixture when a real engine answered.
 *
 * The two subtle ones are `resolveEngineUrl` and `worldFetchBlocked`, which are about a URL that
 * arrives from the engine (`Hello.world_ref.str_url`, §3.1.6) and is root-relative — it is only
 * correct to fetch from the page's own origin when the page and the engine *are* the same origin.
 */

import { describe, expect, it } from "vitest";

import {
  ENGINE_STORAGE_KEY,
  candidateTargets,
  clearOverride,
  engineFlavour,
  normaliseBase,
  probeEngine,
  readOverride,
  resolveEngineTarget,
  resolveEngineUrl,
  worldFetchBlocked,
  writeOverride,
  type StorageLike,
} from "../src/lib/target.js";

/** A `/healthz` responder keyed by base URL; anything unlisted is a network failure. */
function fakeFetch(bodies: Record<string, unknown>, statuses: Record<string, number> = {}): typeof fetch {
  const impl = async (input: unknown): Promise<unknown> => {
    const url = String(input);
    const base = url.replace(/\/healthz$/, "");
    if (!(base in bodies)) throw new Error(`ECONNREFUSED ${url}`);
    const status = statuses[base] ?? 200;
    return {
      ok: status >= 200 && status < 300,
      status,
      json: () => Promise.resolve(bodies[base]),
    };
  };
  return impl as unknown as typeof fetch;
}

function memoryStorage(initial: Record<string, string> = {}): StorageLike & { map: Map<string, string> } {
  const map = new Map(Object.entries(initial));
  return {
    map,
    getItem: (k) => map.get(k) ?? null,
    setItem: (k, v) => void map.set(k, v),
    removeItem: (k) => void map.delete(k),
  };
}

const REAL = { ok: true, engine: "v2xw-server 0.1.0", runs: ["11111111-2222-3333-4444-555555555555"] };
const MOCK = { ok: true, engine: "vwp-mock-server 0.1.0", runs: ["aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"] };

describe("normaliseBase", () => {
  it("drops trailing slashes and leaves the page origin as the empty string", () => {
    expect(normaliseBase("http://127.0.0.1:8787/")).toBe("http://127.0.0.1:8787");
    expect(normaliseBase("http://127.0.0.1:8787///")).toBe("http://127.0.0.1:8787");
    expect(normaliseBase("   ")).toBe("");
  });

  it("converts a WebSocket origin to the HTTP one it shares", () => {
    // `/healthz` and `/world` are HTTP; `VwpClient.endpointUrl` does the reverse conversion itself,
    // so a user who pasted the ws:// URL out of a server banner must still get a working target.
    expect(normaliseBase("ws://127.0.0.1:8787")).toBe("http://127.0.0.1:8787");
    expect(normaliseBase("wss://example.test/")).toBe("https://example.test");
  });
});

describe("engineFlavour", () => {
  it("recognises both implementations by their banner", () => {
    expect(engineFlavour("v2xw-server 0.1.0")).toBe("real");
    expect(engineFlavour("vwp-mock-server 0.1.0")).toBe("mock");
  });

  it("calls anything else unknown rather than guessing", () => {
    expect(engineFlavour("some-other-engine 9.9")).toBe("unknown");
    expect(engineFlavour("")).toBe("unknown");
  });

  it("does not mistake the mock for the real engine on a prefix", () => {
    // "vwp-mock-server" does not start with "v2xw-server", but a looser match (a `includes`, or a
    // check on the first four characters) would put the fixture in the `real` bucket, which is the
    // one mistake this function exists to prevent.
    expect(engineFlavour("vwp-mock-server 0.1.0")).not.toBe("real");
  });
});

describe("probeEngine", () => {
  it("reports the banner and the runs of a reachable engine", async () => {
    const probe = await probeEngine("http://127.0.0.1:8787", fakeFetch({ "http://127.0.0.1:8787": REAL }));
    expect(probe.reachable).toBe(true);
    expect(probe.flavour).toBe("real");
    expect(probe.engine).toBe("v2xw-server 0.1.0");
    expect(probe.runs).toEqual(REAL.runs);
  });

  it("treats a non-200 as unreachable and keeps the status", async () => {
    const probe = await probeEngine(
      "http://127.0.0.1:8787",
      fakeFetch({ "http://127.0.0.1:8787": { error: "unauthorized" } }, { "http://127.0.0.1:8787": 401 }),
    );
    expect(probe.reachable).toBe(false);
    expect(probe.status).toBe(401);
    expect(probe.engine).toContain("401");
  });

  it("refuses a 200 that is not a /healthz body", async () => {
    // An unrelated server on the port — a dev server's index page — answers 200 to everything.
    const probe = await probeEngine("http://127.0.0.1:5173", fakeFetch({ "http://127.0.0.1:5173": { hello: "world" } }));
    expect(probe.reachable).toBe(false);
    expect(probe.engine).toContain("not a VWP");
  });

  it("keeps the failure text when the request throws", async () => {
    const probe = await probeEngine("http://127.0.0.1:9999", fakeFetch({}));
    expect(probe.reachable).toBe(false);
    expect(probe.status).toBe(0);
    expect(probe.engine).toContain("ECONNREFUSED");
  });
});

describe("candidateTargets", () => {
  it("probes the page origin first, then the two defaults, without duplicates", () => {
    const candidates = candidateTargets(null);
    expect(candidates[0].baseUrl).toBe("");
    // The origin is a configuration, not a discovery, so it is not second-guessed.
    expect(candidates[0].authoritative).toBe(true);
    expect(candidates.slice(1).every((c) => c.authoritative !== true)).toBe(true);
    // The real server and the mock both default to :8787, so the list must not carry it twice.
    expect(new Set(candidates.map((c) => c.baseUrl)).size).toBe(candidates.length);
    expect(candidates.some((c) => c.baseUrl === "http://127.0.0.1:8787")).toBe(true);
  });

  it("collapses to one pinned candidate when there is an override", () => {
    const candidates = candidateTargets("http://127.0.0.1:8899");
    expect(candidates).toHaveLength(1);
    expect(candidates[0]).toMatchObject({ baseUrl: "http://127.0.0.1:8899", pinned: true });
  });
});

describe("resolveEngineTarget", () => {
  it("prefers a real engine over a reachable mock", async () => {
    const resolved = await resolveEngineTarget(
      [
        { baseUrl: "http://mock.test", label: "mock", pinned: false },
        { baseUrl: "http://real.test", label: "real", pinned: false },
      ],
      fakeFetch({ "http://mock.test": MOCK, "http://real.test": REAL }),
    );
    expect(resolved.baseUrl).toBe("http://real.test");
    expect(resolved.probe?.flavour).toBe("real");
    // Both were probed: the mock could not be skipped, since its flavour is only known after asking.
    expect(resolved.tried).toHaveLength(2);
  });

  it("short-circuits on the first real engine", async () => {
    const resolved = await resolveEngineTarget(
      [
        { baseUrl: "http://real.test", label: "real", pinned: false },
        { baseUrl: "http://mock.test", label: "mock", pinned: false },
      ],
      fakeFetch({ "http://mock.test": MOCK, "http://real.test": REAL }),
    );
    expect(resolved.baseUrl).toBe("http://real.test");
    expect(resolved.tried).toHaveLength(1);
  });

  it("falls back to the mock when no real engine answers", async () => {
    const resolved = await resolveEngineTarget(
      [
        { baseUrl: "http://gone.test", label: "gone", pinned: false },
        { baseUrl: "http://mock.test", label: "mock", pinned: false },
      ],
      fakeFetch({ "http://mock.test": MOCK }),
    );
    expect(resolved.baseUrl).toBe("http://mock.test");
    expect(resolved.probe?.flavour).toBe("mock");
  });

  it("takes the page's own origin whatever it turns out to be, without scanning for a real engine", async () => {
    // This is what keeps the end-to-end suite working *and* honest: the Vite proxy points at the
    // fixture, that is a decision, and a page that then fetched `http://127.0.0.1:8787/healthz`
    // would be scanning a port it was not pointed at — a cross-origin request, a console error, and
    // an override of the configuration. The flavour is still reported.
    const resolved = await resolveEngineTarget(
      [
        { baseUrl: "", label: "origin", pinned: false, authoritative: true },
        { baseUrl: "http://real.test", label: "real", pinned: false },
      ],
      fakeFetch({ "": MOCK, "http://real.test": REAL }),
    );
    expect(resolved.baseUrl).toBe("");
    expect(resolved.probe?.flavour).toBe("mock");
    expect(resolved.tried).toHaveLength(1);
  });

  it("still discovers a real engine when the page's own origin serves nothing", async () => {
    const resolved = await resolveEngineTarget(
      [
        { baseUrl: "", label: "origin", pinned: false, authoritative: true },
        { baseUrl: "http://real.test", label: "real", pinned: false },
      ],
      fakeFetch({ "http://real.test": REAL }),
    );
    expect(resolved.baseUrl).toBe("http://real.test");
  });

  it("takes a pinned mock without looking for a real engine", async () => {
    // This is what keeps the Playwright suite on the fixture: the pin is a decision, not a hint.
    const resolved = await resolveEngineTarget(
      [{ baseUrl: "http://mock.test", label: "mock", pinned: true }],
      fakeFetch({ "http://mock.test": MOCK, "http://real.test": REAL }),
    );
    expect(resolved.baseUrl).toBe("http://mock.test");
    expect(resolved.pinned).toBe(true);
  });

  it("returns the first candidate when nothing is reachable, so the error names the right target", async () => {
    const resolved = await resolveEngineTarget(
      [
        { baseUrl: "", label: "origin", pinned: false },
        { baseUrl: "http://gone.test", label: "gone", pinned: false },
      ],
      fakeFetch({}),
    );
    expect(resolved.baseUrl).toBe("");
    expect(resolved.probe?.reachable).toBe(false);
    expect(resolved.tried).toHaveLength(2);
  });
});

describe("the override", () => {
  it("reads `?engine=` and makes it sticky", () => {
    const storage = memoryStorage();
    expect(readOverride("?engine=http://127.0.0.1:8899/", storage)).toBe("http://127.0.0.1:8899");
    expect(storage.map.get(ENGINE_STORAGE_KEY)).toBe("http://127.0.0.1:8899");
  });

  it("treats `?engine=origin` as a pin on the page's own origin", () => {
    const storage = memoryStorage({ [ENGINE_STORAGE_KEY]: "http://elsewhere.test" });
    expect(readOverride("?engine=origin", storage)).toBe("");
    expect(storage.map.get(ENGINE_STORAGE_KEY)).toBe("");
  });

  it("falls back to storage, and to null when there is nothing", () => {
    expect(readOverride("", memoryStorage({ [ENGINE_STORAGE_KEY]: "http://saved.test" }))).toBe("http://saved.test");
    expect(readOverride("", memoryStorage())).toBeNull();
    expect(readOverride("", null)).toBeNull();
  });

  it("survives a storage that throws, which is what private browsing does", () => {
    const hostile: StorageLike = {
      getItem: () => {
        throw new Error("storage disabled");
      },
      setItem: () => {
        throw new Error("storage disabled");
      },
      removeItem: () => {
        throw new Error("storage disabled");
      },
    };
    expect(readOverride("", hostile)).toBeNull();
    // The query override still applies to this page even though it could not be persisted.
    expect(readOverride("?engine=http://pinned.test", hostile)).toBe("http://pinned.test");
    expect(() => writeOverride("http://x.test", hostile)).not.toThrow();
    expect(() => clearOverride(hostile)).not.toThrow();
  });

  it("round-trips through write and clear", () => {
    const storage = memoryStorage();
    writeOverride("http://127.0.0.1:8787/", storage);
    expect(readOverride("", storage)).toBe("http://127.0.0.1:8787");
    clearOverride(storage);
    expect(readOverride("", storage)).toBeNull();
  });
});

describe("resolveEngineUrl", () => {
  it("joins the engine's root-relative world URL onto the engine's base", () => {
    // §3.1.6 gives `/world/{hash}.vwb`; against a pinned engine the page's origin is the wrong root.
    expect(resolveEngineUrl("http://127.0.0.1:8787", "/world/abc.vwb")).toBe("http://127.0.0.1:8787/world/abc.vwb");
    expect(resolveEngineUrl("http://127.0.0.1:8787", "world/abc.vwb")).toBe("http://127.0.0.1:8787/world/abc.vwb");
  });

  it("leaves the URL alone for the same-origin case and for an absolute URL", () => {
    expect(resolveEngineUrl("", "/world/abc.vwb")).toBe("/world/abc.vwb");
    expect(resolveEngineUrl("http://127.0.0.1:8787", "https://cdn.test/world/abc.vwb")).toBe(
      "https://cdn.test/world/abc.vwb",
    );
  });
});

describe("worldFetchBlocked", () => {
  it("is true only when the engine is on another origin", () => {
    // §1.1 makes both servers set `cross-origin-resource-policy: same-origin`, so this is a fact
    // about the response headers and not about CORS configuration.
    expect(worldFetchBlocked("http://127.0.0.1:8787", "http://127.0.0.1:5173")).toBe(true);
    expect(worldFetchBlocked("http://127.0.0.1:5173", "http://127.0.0.1:5173")).toBe(false);
    expect(worldFetchBlocked("", "http://127.0.0.1:5173")).toBe(false);
  });

  it("does not claim a block it cannot establish", () => {
    expect(worldFetchBlocked("not a url", "http://127.0.0.1:5173")).toBe(false);
  });
});
