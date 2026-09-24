/**
 * The real engine as a child process, and the page operations the engine suite is written in.
 *
 * The engine is not a Playwright `webServer` because the suite kills it with the page open and
 * starts it again; a `webServer` is started once and must stay up.
 */

import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { expect, type Page } from "@playwright/test";

export const REPO = resolve(dirname(fileURLToPath(import.meta.url)), "../../../..");
export const ENGINE_PORT = Number(process.env.VWP_ENGINE_PORT ?? "8789");
export const ENGINE_BIN = process.env.VWP_ENGINE_BIN ?? join(REPO, "target/debug/v2xw-server");

/**
 * Two small scenarios in a directory of their own, so the page's "Ready-made scenarios" list is
 * exactly these two and switching between them is a real scenario change (another name, another
 * length, another fleet). Both are `phase1-grid.yaml` — a procedural world, so no map import — with
 * the lines named here replaced.
 */
export function writeScenarios(): { a: string; b: string } {
  const source = readFileSync(join(REPO, "scenarios/phase1-grid.yaml"), "utf8");
  const dir = join(tmpdir(), `vwp-engine-e2e-${process.pid}`);
  mkdirSync(dir, { recursive: true });
  const variant = (name: string, seconds: number, rate: number): string => {
    const text = source
      .replace("name: phase1-grid", `name: ${name}`)
      .replace("duration_s: 60.0", `duration_s: ${seconds}.0`)
      .replace("rate_veh_per_h: 30.0", `rate_veh_per_h: ${rate}.0`);
    const path = join(dir, `${name}.yaml`);
    writeFileSync(path, text);
    return path;
  };
  return { a: variant("e2e-grid-a", 6, 3000), b: variant("e2e-grid-b", 4, 600) };
}

/** One engine process, started from the repository root as a user would start it. */
export class EngineProcess {
  #child: ChildProcess | null = null;

  /** `port` defaults to the one the page's proxy targets; a test that interposes on the
   * connection (resume.spec.ts) starts the engine elsewhere and listens there itself. */
  constructor(
    readonly scenario: string,
    readonly port: number = ENGINE_PORT,
    readonly speed: string = "0",
  ) {}

  get pid(): number {
    const pid = this.#child?.pid;
    if (pid === undefined) throw new Error("the engine is not running");
    return pid;
  }

  async start(): Promise<void> {
    this.#child = spawn(
      ENGINE_BIN,
      ["--scenario", this.scenario, "--port", String(this.port), "--paused", "--speed", this.speed, "--quiet"],
      { cwd: REPO, stdio: ["ignore", "ignore", "pipe"] },
    );
    let stderr = "";
    this.#child.stderr?.on("data", (d: Buffer) => {
      stderr += d.toString();
    });
    const deadline = Date.now() + 120_000;
    while (Date.now() < deadline) {
      if (this.#child.exitCode !== null) throw new Error(`the engine exited: ${stderr}`);
      try {
        const res = await fetch(`http://127.0.0.1:${this.port}/healthz`);
        if (res.ok) return;
      } catch {
        /* not up yet */
      }
      await new Promise((r) => setTimeout(r, 250));
    }
    throw new Error(`the engine did not come up in 120 s: ${stderr}`);
  }

  /** Ends the process: politely, or with SIGKILL for the "the engine died" test. */
  async stop(signal: NodeJS.Signals = "SIGINT"): Promise<void> {
    const child = this.#child;
    if (!child || child.exitCode !== null) return;
    const exited = new Promise<void>((r) => child.once("exit", () => r()));
    child.kill(signal);
    const timeout = new Promise<void>((r) => setTimeout(r, 10_000));
    await Promise.race([exited, timeout]);
    if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
    this.#child = null;
  }

  /** Resident set size, kilobytes. */
  rssKb(): number {
    return Number(execFileSync("ps", ["-o", "rss=", "-p", String(this.pid)]).toString().trim());
  }

  /** How many threads the process has. */
  threads(): number {
    return execFileSync("ps", ["-M", "-p", String(this.pid)]).toString().trim().split("\n").length - 1;
  }
}

/** `run.status`, over HTTP so it answers whatever the socket is doing. */
export interface Status {
  state: string;
  t_ns: number;
  t_end_ns: number;
  generation: number;
  speed: number;
  scenario_hash: string;
  staged_hash: string | null;
  engine: {
    kernel_threads: number;
    output_digest: string | null;
    stats: {
      actors_seen: number;
      tx_by_type: Record<string, number>;
      rx_attempts: number;
      rx_ok: number;
      mean_rssi_dbm: number | null;
    };
  };
}

export async function status(page: Page): Promise<Status> {
  return page.evaluate(async () => {
    const engine = window.__vwpStudio?.engine as unknown as {
      requestHttp(m: string, p: unknown, o: unknown): Promise<unknown>;
    };
    return (await engine.requestHttp("run.status", {}, { quiet: true })) as never;
  });
}

/** Open the page and wait for the stream. */
export async function open(page: Page): Promise<void> {
  await page.goto("/");
  await streaming(page);
  // The engine's own settings list, not the built-in fallback.
  await expect(page.getByTestId("schema-source")).toContainText("settings are the ones this engine accepts");
}

export async function streaming(page: Page, timeout = 60_000): Promise<void> {
  await expect(page.getByTestId("connection-state")).toHaveAttribute("data-state", "streaming", { timeout });
}

/**
 * Put a value into one setting of the form, by its JSON Pointer.
 *
 * The filter box is used to reach it, because the groups are collapsible and a field in a closed
 * group is not interactable — which is also how a user finds one setting among a hundred.
 */
export async function setField(page: Page, pointer: string, value: string): Promise<void> {
  const filter = page.getByTestId("settings-filter");
  await filter.fill(pointer.split("/").pop() ?? pointer);
  const row = page.locator(`[data-testid="setting"][data-pointer="${pointer}"]`);
  await expect(row).toBeVisible();
  const select = row.locator("select");
  if ((await select.count()) > 0) {
    await select.selectOption(value);
  } else {
    const box = row.locator("input, textarea").first();
    await box.fill(value);
  }
  await filter.fill("");
}

/**
 * Press Run and wait for the run it starts to finish; returns its final status.
 *
 * "The run it starts" is decided by the generation: a status from the previous run, finished,
 * must not be mistaken for this one — which is exactly the mistake an earlier version of this
 * helper made, reading "finished" off the run that had just ended.
 */
export async function runToEnd(page: Page, button = "run-start", timeoutMs = 180_000): Promise<Status> {
  const before = (await status(page)).generation;
  await page.getByTestId(button).click();
  const deadline = Date.now() + timeoutMs;
  let last: Status = await status(page);
  while (Date.now() < deadline) {
    last = await status(page);
    if (last.generation > before && last.state === "finished" && last.engine.output_digest !== null) return last;
    await page.waitForTimeout(250);
  }
  throw new Error(`the run did not finish: ${JSON.stringify(last)}`);
}

/** The tab's live JS heap after a collection, bytes. */
export async function heapBytes(page: Page): Promise<number> {
  return page.evaluate(() => {
    (globalThis as { gc?: () => void }).gc?.();
    return (performance as unknown as { memory?: { usedJSHeapSize: number } }).memory?.usedJSHeapSize ?? 0;
  });
}
