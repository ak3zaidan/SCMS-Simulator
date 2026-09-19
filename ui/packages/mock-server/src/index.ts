#!/usr/bin/env node
/**
 * CLI for the VWP v1 mock engine server.
 *
 *     node dist/index.js --actors 500 --port 8787
 */

import { pathToFileURL } from "node:url";

import { MockEngineServer, type MockServerOptions } from "./server.js";
import type { GeoBbox } from "./geo.js";

export { MockEngineServer } from "./server.js";
export type { MockServerOptions } from "./server.js";
export { MANHATTAN_BBOX, generateManhattan } from "./manhattan.js";
export type { GeneratedWorld, ManhattanOptions } from "./manhattan.js";
export { MockRun, ACTOR_CLASSES, ACTOR_NODE_BASE, signalPlanAt } from "./sim.js";
export type { MockRunOptions, Profile } from "./sim.js";
export { projectionForBbox } from "./geo.js";
export type { EnuProjection, GeoBbox } from "./geo.js";

const USAGE = `vwp-mock-server — a VWP v1 engine fixture (docs/protocol/vwp-v1.md)

  node dist/index.js [options]

  --actors <n>     actors to drive (default 200; 5000 is the stress point)
  --port <n>       TCP port (default 8787)
  --host <addr>    bind address (default 127.0.0.1)
  --speed <x>      multiple of real time (default 1; 0 = as fast as the timer allows)
  --seed <n>       world and traffic seed (default 20260918)
  --bbox <l,b,r,t> WGS-84 bbox (default the midtown-Manhattan one)
  --paused         start the run paused at t = 0
  --quiet          do not print the banner
  --help           this message
`;

/** Parse `process.argv`-style arguments into {@link MockServerOptions}. */
export function parseArgs(argv: readonly string[]): MockServerOptions & { help?: boolean } {
  const options: Record<string, unknown> = {};
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    const next = (): string => {
      const value = argv[i + 1];
      if (value === undefined) throw new Error(`${arg} needs a value`);
      i += 1;
      return value;
    };
    switch (arg) {
      case "--actors": options.actors = Number(next()); break;
      case "--port": options.port = Number(next()); break;
      case "--host": options.host = next(); break;
      case "--speed": options.speed = Number(next()); break;
      case "--seed": options.seed = Number(next()); break;
      case "--bbox": {
        const parts = next().split(",").map(Number);
        if (parts.length !== 4 || parts.some((n) => !Number.isFinite(n))) throw new Error("--bbox needs min_lon,min_lat,max_lon,max_lat");
        options.bbox = parts as unknown as GeoBbox;
        break;
      }
      case "--paused": options.paused = true; break;
      case "--quiet": options.quiet = true; break;
      case "--help":
      case "-h": options.help = true; break;
      default:
        if (arg.startsWith("--")) throw new Error(`unknown option ${arg}`);
    }
  }
  return options as MockServerOptions & { help?: boolean };
}

/** Start a server from parsed options and print a banner. */
export async function main(argv: readonly string[] = process.argv.slice(2)): Promise<MockEngineServer> {
  const options = parseArgs(argv);
  if (options.help) {
    process.stdout.write(USAGE);
    process.exit(0);
  }
  const server = new MockEngineServer(options);
  const address = await server.start();
  if (!options.quiet) {
    const c = server.world.counts;
    process.stdout.write(
      [
        `vwp-mock-server listening on ${address.httpUrl}`,
        `  stream   ${address.wsUrl}?compress=none&v=1   (subprotocol vwp.v1)`,
        `  world    ${address.httpUrl}/world/${server.worldHashHex}.vwb`,
        `  json     ${address.httpUrl}/world/${server.worldHashHex}.json`,
        `  rpc      POST ${address.httpUrl}/rpc   schema ${address.httpUrl}/rpc/schema`,
        `  run      ${server.runId}`,
        `  world    ${c.lanes} lanes, ${c.buildings} buildings, ${c.junctions} junctions, ${c.signals} signals, ${c.sites} sites, ${(c.bytes / 1024).toFixed(0)} KiB`,
        `  traffic  ${server.run.actorCount} actors, keyframes every 1 s, deltas every 100 ms`,
        "",
      ].join("\n"),
    );
  }
  const shutdown = (): void => {
    void server.stop().then(() => process.exit(0));
  };
  process.on("SIGINT", shutdown);
  process.on("SIGTERM", shutdown);
  return server;
}

const entry = process.argv[1];
const isDirectRun = entry !== undefined && import.meta.url === pathToFileURL(entry).href;
if (isDirectRun || process.env.VWP_MOCK_SERVER_AUTOSTART === "1") {
  void main().catch((err: unknown) => {
    process.stderr.write(`${err instanceof Error ? err.message : String(err)}\n`);
    process.exit(1);
  });
}
