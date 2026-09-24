/**
 * `@vwp/protocol` — a VWP v1 client for the V2X World Simulator UI.
 *
 * The wire protocol is docs/protocol/vwp-v1.md. Every decoder here is written from its tables;
 * the §9 hex dumps are golden tests in `test/spec-vectors.test.ts`.
 *
 * ```ts
 * import { VwpClient, decodeWorld } from "@vwp/protocol";
 *
 * const client = new VwpClient({ url: "ws://127.0.0.1:8787", compress: "none" });
 * client.onKeyframe(() => render(client.poses.positions, client.poses.count));
 * const hello = await client.connect();
 * const world = decodeWorld(await (await fetch(`/world/${bytesToHex(hello.worldHash)}.vwb`)).arrayBuffer());
 * ```
 */

export * from "./frame.js";
export * from "./messages.js";
export * from "./pose.js";
export * from "./profile.js";
export * from "./slots.js";
export * from "./world.js";
export * from "./encode.js";
export * from "./rpc.js";
export * from "./feed.js";
export * from "./client.js";
export * from "./worker.js";

/**
 * The protocol version this package implements. v1.1 added `view.follow {feed}` and the
 * `node.feed` notification (§8.4, additive).
 */
export const VWP_PROTOCOL_VERSION = "1.1";
