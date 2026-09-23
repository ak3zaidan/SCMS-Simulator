/**
 * "socket closed before Hello (code 1006)" — the page destroying its own connection.
 *
 * The owner's report: the first few runs worked, then Run and Run again stopped doing anything.
 * The progress bar still moved (the 2 s `run.status` poll over HTTP keeps working) but no vehicle
 * ever appeared, and the header read *"That did not work: socket closed before Hello (code 1006)"*.
 *
 * The engine was never at fault. Measured against the live server while it was failing:
 * `run.start` over HTTP rewound the run correctly, and a raw socket opened from node — both
 * straight to `:8787` and through the Vite proxy on `:5173` — received 89 and 88 frames in eight
 * seconds. Only the browser failed, and its console named the cause exactly: *"WebSocket is closed
 * before the connection is established"*. That is a client aborting its own socket mid-handshake,
 * not a server refusing one.
 *
 * `connect()` opened with `this.disconnect()`. A teardown is not a guard: two overlapping calls had
 * the second close the first one's socket while it was still handshaking. React's StrictMode
 * double-invoke is the usual way in, and the comment at the call site asserted the double-invoke
 * was "absorbed by the guard in `engine.connect()`" — a guard that did not exist.
 *
 * Whether it bit depended on how long the `/healthz` probe took, which is why it looked
 * intermittent rather than broken: a fast probe let the two calls overlap, a slow one let the first
 * finish first.
 *
 * So the property: concurrent callers for one origin share one attempt, and a genuine change of
 * engine still supersedes.
 */
import { describe, expect, it, vi } from "vitest";

/** The shape under test, reduced to the decision: coalesce by origin, supersede on change. */
class Connector {
  inFlight: { url: string; promise: Promise<string> } | null = null;
  attempts = 0;
  teardowns = 0;
  aborted = 0;

  /** Stands in for `#connectOnce`: tears down first, then handshakes over two ticks. */
  async once(url: string): Promise<string> {
    this.attempts++;
    const mine = this.attempts;
    this.teardowns++;
    this.inFlightSocket = mine;
    await Promise.resolve();
    await Promise.resolve();
    // The socket this attempt opened was closed by a later teardown while it handshook.
    if (this.inFlightSocket !== mine) {
      this.aborted++;
      throw new Error("WebSocket is closed before the connection is established");
    }
    return `hello:${url}`;
  }
  private inFlightSocket = 0;

  async connect(url: string): Promise<string> {
    const f = this.inFlight;
    if (f && f.url === url) return f.promise;
    const promise = this.once(url).finally(() => {
      if (this.inFlight?.promise === promise) this.inFlight = null;
    });
    this.inFlight = { url, promise };
    return promise;
  }
}

describe("connect() coalesces concurrent callers", () => {
  it("StrictMode's double-invoke opens one socket, not two, and neither is aborted", async () => {
    const c = new Connector();
    const [a, b] = await Promise.all([c.connect("http://x"), c.connect("http://x")]);
    expect(a).toBe("hello:http://x");
    expect(b).toBe("hello:http://x");
    expect(c.attempts).toBe(1);
    expect(c.aborted).toBe(0);
  });

  it("without the guard the second call aborts the first mid-handshake", async () => {
    // The fault as it shipped: every call tears down, so the first attempt's socket dies.
    const c = new Connector();
    const first = c.once("http://x");
    const second = c.once("http://x");
    await expect(first).rejects.toThrow(/closed before the connection is established/);
    await expect(second).resolves.toBe("hello:http://x");
    expect(c.aborted).toBe(1);
  });

  it("a different origin is a deliberate change of engine, so it supersedes", async () => {
    const c = new Connector();
    await c.connect("http://x");
    await c.connect("http://y");
    expect(c.attempts).toBe(2);
  });

  it("the in-flight record is released, so a later connect is not joined to a dead attempt", async () => {
    const c = new Connector();
    await c.connect("http://x");
    expect(c.inFlight).toBeNull();
    await c.connect("http://x");
    expect(c.attempts).toBe(2);
  });
});
