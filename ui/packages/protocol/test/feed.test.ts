/**
 * `node.feed` conformance: the TypeScript definitions against the vector the Rust server is held to.
 *
 * `docs/protocol/vectors/node-feed-v1.json` is a real push, captured from a grid run by
 * `crates/v2xw-server/tests/feed.rs` (`VWP_BLESS_FEED_VECTOR=1`), whose test also fails whenever the
 * server's push stops having exactly that shape. This file closes the loop from the other side: the
 * vector must parse with {@link parseNodeFeed}, carry {@link NODE_FEED_VERSION}, and have exactly the
 * members the TypeScript interfaces declare — so a member added on either side without the other
 * fails one of the two suites.
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import {
  NODE_FEED_VERSION,
  VWP_NOTIFICATIONS,
  parseNodeFeed,
  type FeedQueue,
  type FeedReceived,
  type FeedSent,
  type NodeFeedNotification,
} from "../src/index.js";

const VECTOR = fileURLToPath(new URL("../../../../docs/protocol/vectors/node-feed-v1.json", import.meta.url));
const raw: unknown = JSON.parse(readFileSync(VECTOR, "utf8"));

/**
 * The members each interface declares, spelled out once. `satisfies` makes the compiler check the
 * lists against the interfaces: a key missing here, or one that is not a member, is a type error.
 */
const TOP = [
  "v", "node", "t_ns", "since_ns", "reset", "sent", "received", "omitted", "shed", "undetected", "history_ns", "queues",
] as const satisfies readonly (keyof NodeFeedNotification)[];
const SENT = ["msg", "t_ns", "type", "bytes", "radio", "signer", "pseudonym", "timing", "decoded"] as const satisfies readonly (keyof FeedSent)[];
const RECEIVED_DELIVERED = [
  "msg", "t_ns", "type", "outcome", "cause", "verification", "rssi_dbm", "sinr_db", "bytes_on_wire", "e2e_ms", "stages_ms",
  "from", "dist_m", "decoded",
] as const satisfies readonly (keyof FeedReceived)[];
const QUEUE = [
  "id", "label", "what", "depth", "in_service", "served", "wait_p50_ms", "wait_p95_ms", "drops", "waiting", "waiting_omitted",
  "reported_depth",
] as const satisfies readonly (keyof FeedQueue)[];

const keys = (o: unknown): string[] => Object.keys(o as Record<string, unknown>).sort();

describe("node.feed v1 — the shared vector", () => {
  it("parses, and carries the version this package reads", () => {
    const r = parseNodeFeed(raw);
    expect(r.ok, r.ok ? "" : r.reason).toBe(true);
    expect(NODE_FEED_VERSION).toBe(1);
    expect((raw as NodeFeedNotification).v).toBe(NODE_FEED_VERSION);
    expect(VWP_NOTIFICATIONS).toContain("node.feed");
  });

  it("has exactly the members the interfaces declare", () => {
    const v = raw as NodeFeedNotification;
    expect(keys(v)).toEqual([...TOP].sort());
    expect(keys(v.sent[0])).toEqual([...SENT].sort());
    const delivered = v.received.find((r) => r.outcome === "delivered");
    expect(delivered, "the vector holds a delivered reception").toBeDefined();
    expect(keys(delivered)).toEqual([...RECEIVED_DELIVERED].sort());
    for (const q of v.queues.list) {
      // `drops_note` is the one optional member: only the transmit queue carries it.
      expect(keys(q).filter((k) => k !== "drops_note")).toEqual([...QUEUE].sort());
    }
    expect(v.queues.list.map((q) => q.id)).toEqual(["rx", "verify", "app", "tx", "crl"]);
  });

  it("decodes a BSM whose spans tile its SPDU and whose signer is the frame's pseudonym", () => {
    const s = (raw as NodeFeedNotification).sent[0];
    const d = s.decoded;
    expect(d.hex?.length).toBe(2 * (d.spdu_bytes ?? -1));
    let at = 0;
    for (const span of d.spans ?? []) {
      expect(span.start).toBe(at);
      at = span.end;
    }
    expect(at).toBe(d.spdu_bytes);
    expect(d.security?.signer.hashed_id8).toBe(s.pseudonym);
    const keysOf = new Set((d.message?.fields ?? []).map((f) => f.k));
    for (const k of ["msg_cnt", "temp_id", "sec_mark", "lat", "lon", "elev", "speed", "heading", "accel_long", "brakes_wheels", "width", "length"]) {
      expect(keysOf.has(k), `BSM field ${k}`).toBe(true);
    }
  });

  it("refuses another version and a push missing what a row is built from", () => {
    expect(parseNodeFeed({ ...(raw as object), v: 2 })).toMatchObject({ ok: false });
    const noSent = { ...(raw as object) } as Record<string, unknown>;
    delete noSent.sent;
    expect(parseNodeFeed(noSent)).toMatchObject({ ok: false });
    const badRow = JSON.parse(JSON.stringify(raw)) as { sent: Record<string, unknown>[] };
    delete badRow.sent[0].decoded;
    expect(parseNodeFeed(badRow)).toMatchObject({ ok: false });
  });
});
