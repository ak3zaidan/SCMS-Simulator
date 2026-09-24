/**
 * A network drop mid-run is invisible in the page: the stream resumes where it left off.
 *
 * A TCP proxy sits between the page's dev server and the engine, and the test cuts every
 * connection through it while a run is playing — no WebSocket close frame on either side, which
 * is what a dropped link looks like. The page reconnects on its own (§1.4 backoff), presenting the
 * session token its `Hello` issued and the `seq` it expects next, and the engine replays exactly
 * what it missed.
 *
 * What the page received is recorded frame by frame, so the claim is checked where the owner
 * would see it: the canonical `seq` the page applied has no gap and no duplicate across the drop,
 * the reconnect `Hello` says `HELLO_RESUMED`, and the page logged no error.
 */

import { expect, test, type Page } from "@playwright/test";
import { createServer, connect, type Server, type Socket } from "node:net";

import { ENGINE_PORT, EngineProcess, open, status, writeScenarios } from "./support.js";

/** The engine listens here; the page's proxy targets ENGINE_PORT, where the cuttable proxy is. */
const BEHIND = ENGINE_PORT + 1000;

class CuttableProxy {
  #server: Server | null = null;
  #sockets = new Set<Socket>();

  async start(listen: number, upstream: number): Promise<void> {
    this.#server = createServer((inbound) => {
      const outbound = connect(upstream, "127.0.0.1");
      this.#sockets.add(inbound);
      this.#sockets.add(outbound);
      inbound.pipe(outbound);
      outbound.pipe(inbound);
      const drop = (): void => {
        inbound.destroy();
        outbound.destroy();
        this.#sockets.delete(inbound);
        this.#sockets.delete(outbound);
      };
      inbound.on("error", drop);
      outbound.on("error", drop);
      inbound.on("close", drop);
      outbound.on("close", drop);
    });
    await new Promise<void>((r) => this.#server?.listen(listen, "127.0.0.1", () => r()));
  }

  /** Destroys every open connection: a reset, not a close handshake. */
  cut(): number {
    const n = this.#sockets.size;
    for (const s of this.#sockets) s.resetAndDestroy();
    this.#sockets.clear();
    return n;
  }

  async stop(): Promise<void> {
    this.cut();
    await new Promise<void>((r) => this.#server?.close(() => r()) ?? r());
  }
}

const scenarios = writeScenarios();
// A long run at real time, so the drop lands in the middle of it.
const engine = new EngineProcess(scenarios.a, BEHIND, "1");
const proxy = new CuttableProxy();

test.beforeAll(async () => {
  await engine.start();
  await proxy.start(ENGINE_PORT, BEHIND);
});
test.afterAll(async () => {
  await proxy.stop();
  await engine.stop();
});

/** Records every canonical frame's seq and every Hello the page's client decodes. */
async function recordStream(page: Page): Promise<void> {
  await page.evaluate(() => {
    const studio = window.__vwpStudio?.engine as unknown as {
      client: {
        on(e: "message", f: (m: { kind: string; header: { seq: bigint; msgType: number }; helloFlags?: number; resumeSeq?: bigint }) => void): () => void;
        onState(f: (s: string) => void): () => void;
      };
    };
    const log = { seqs: [] as number[], hellos: [] as { flags: number; resumeSeq: number }[], states: [] as string[] };
    (window as unknown as { __streamLog: typeof log }).__streamLog = log;
    studio.client.on("message", (m) => {
      if (m.kind === "hello") {
        log.hellos.push({ flags: m.helloFlags ?? 0, resumeSeq: Number(m.resumeSeq ?? 0n) });
        return;
      }
      // Canonical frames: everything that carries a seq in the stream (not Error / Bye).
      if (m.kind === "error" || m.kind === "bye" || m.kind === "world-chunk") return;
      log.seqs.push(Number(m.header.seq));
    });
    studio.client.onState((s) => log.states.push(s));
  });
}

test("a dropped connection resumes with no gap, no duplicate and no error", async ({ page }) => {
  // The run in scenario a is 6 s: long enough at real time for a drop in its middle.
  await open(page);
  await recordStream(page);
  await page.getByTestId("primary-action").click();
  await expect.poll(async () => (await status(page)).state, { timeout: 30_000 }).toBe("running");
  await expect.poll(async () => (await status(page)).t_ns, { timeout: 30_000 }).toBeGreaterThan(1_500_000_000);

  const cut = proxy.cut();
  expect(cut, "there were live connections to cut").toBeGreaterThan(0);

  // The page notices, backs off, reconnects and resumes on its own.
  await expect(page.getByTestId("connection-state")).toHaveAttribute("data-state", "streaming", { timeout: 30_000 });
  await expect.poll(async () => (await status(page)).state, { timeout: 60_000 }).toBe("finished");
  await page.waitForTimeout(1_000);

  const log = await page.evaluate(
    () => (window as unknown as { __streamLog: { seqs: number[]; hellos: { flags: number; resumeSeq: number }[]; states: string[] } }).__streamLog,
  );
  // At least one reconnect happened, and its Hello resumed the stream.
  expect(log.states, "the page went through a reconnect").toContain("reconnecting");
  const resumed = log.hellos.filter((h) => (h.flags & 0x20) !== 0);
  expect(resumed.length, `a resumed Hello arrived: ${JSON.stringify(log.hellos)}`).toBeGreaterThan(0);
  // Every seq once, in order, from the first to the last the page saw.
  const gaps: string[] = [];
  for (let i = 1; i < log.seqs.length; i++) {
    if (log.seqs[i] !== log.seqs[i - 1] + 1) gaps.push(`${log.seqs[i - 1]} → ${log.seqs[i]}`);
  }
  expect(gaps, `the seq the page applied has no gap and no duplicate (${log.seqs.length} frames)`).toEqual([]);
  expect(resumed[0].resumeSeq).toBeGreaterThan(0);

  // The page said nothing was wrong: no error line, and the stream state never failed.
  const errors = await page.evaluate(() => {
    const hook = (window as unknown as { __vwpStudio?: { logs(): { level: string; target: string; message: string }[] } }).__vwpStudio;
    return (hook?.logs() ?? []).filter((l) => l.level === "error").map((l) => `${l.target}: ${l.message}`);
  });
  expect(errors).toEqual([]);
  expect(log.states).not.toContain("failed");
  // eslint-disable-next-line no-console -- the measured numbers are the evidence this test reports
  console.log(
    `cut ${cut} connections; the page applied ${log.seqs.length} frames, seq ${log.seqs[0]} … ${log.seqs.at(-1)}, ` +
      `with no gap or duplicate; resumed at seq ${resumed[0].resumeSeq}; states ${log.states.join(" → ")}`,
  );
});
