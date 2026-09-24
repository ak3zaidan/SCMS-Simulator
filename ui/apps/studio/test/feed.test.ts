/**
 * The message panel's model (`lib/feed.ts`) and its store actions.
 *
 * The case that drove the model: the integrator opened a message row, and in one pass its fields
 * showed and in the next nothing did. The detail used to be drawn from whichever row carried the
 * opened key among the newest refresh's first twelve rows, and a 10 Hz BSM stream refreshed once a
 * second pushed the opened row out of those twelve one to two seconds after the click — earlier or
 * later depending on where in the refresh cycle the click fell. The detail is now a pinned copy of the
 * entry, and these tests hold it there through pushes, the ring and the filters.
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { beforeEach, describe, expect, it } from "vitest";

import type { FeedReceived, FeedSent, NodeFeedNotification } from "@vwp/protocol";

import { EMPTY_FEED, applyPush, followFeed, hexRows, keyOf, openMessage, setPaused, visibleRows } from "../src/lib/feed.js";
import { StudioEngine } from "../src/state/engine.js";
import { useStudio } from "../src/state/store.js";

const VECTOR = JSON.parse(
  readFileSync(fileURLToPath(new URL("../../../../docs/protocol/vectors/node-feed-v1.json", import.meta.url)), "utf8"),
) as NodeFeedNotification;

const TEMPLATE_SENT = VECTOR.sent[0];
const TEMPLATE_RX = VECTOR.received[0];

function sent(msg: number, tNs: number, type = "bsm"): FeedSent {
  return { ...TEMPLATE_SENT, msg, t_ns: tNs, type };
}

function received(msg: number, tNs: number, outcome: FeedReceived["outcome"] = "delivered"): FeedReceived {
  return { ...TEMPLATE_RX, msg, t_ns: tNs, outcome, cause: outcome === "lost" ? "collision" : null };
}

/** One push of `n` BSMs at 10 Hz starting at message `first`, newest first as the engine sends them. */
function push(node: number, first: number, n: number, extra: Partial<NodeFeedNotification> = {}): NodeFeedNotification {
  const rows = Array.from({ length: n }, (_, i) => sent(first + i, (first + i) * 100_000_000)).reverse();
  return {
    ...VECTOR,
    node,
    t_ns: (first + n) * 100_000_000,
    reset: false,
    sent: rows,
    received: [received(first, first * 100_000_000 + 5)],
    omitted: { sent: 0, received: 0 },
    ...extra,
  };
}

describe("the feed model", () => {
  it("keeps the newest first, without duplicates, bounded by the ring, counting what it drops", () => {
    let f = followFeed(EMPTY_FEED, 7);
    f = applyPush(f, push(7, 0, 5));
    f = applyPush(f, push(7, 3, 5)); // overlaps 3 and 4
    expect(f.sent.map((e) => e.msg)).toEqual([7, 6, 5, 4, 3, 2, 1, 0]);
    f = applyPush(f, push(7, 8, 5), 10);
    expect(f.sent).toHaveLength(10);
    expect(f.sent[0].msg).toBe(12);
    expect(f.dropped.sent).toBe(3);
    expect(f.pushes).toBe(3);
  });

  it("ignores a push for another node, and a reset push starts over", () => {
    let f = followFeed(EMPTY_FEED, 7);
    f = applyPush(f, push(7, 0, 3));
    expect(applyPush(f, push(8, 10, 3))).toBe(f);
    f = applyPush(f, push(7, 50, 2, { reset: true }));
    expect(f.sent.map((e) => e.msg)).toEqual([51, 50]);
  });

  it("adds up what the pushes left out", () => {
    let f = followFeed(EMPTY_FEED, 7);
    f = applyPush(f, push(7, 0, 2, { omitted: { sent: 4, received: 9 } }));
    f = applyPush(f, push(7, 2, 2, { omitted: { sent: 1, received: 0 } }));
    expect(f.omitted).toEqual({ sent: 5, received: 9 });
  });

  it("holds what arrives while paused and applies it on resume", () => {
    let f = applyPush(followFeed(EMPTY_FEED, 7), push(7, 0, 2));
    f = setPaused(f, true);
    f = applyPush(f, push(7, 2, 2));
    f = applyPush(f, push(7, 4, 2));
    expect(f.sent.map((e) => e.msg)).toEqual([1, 0]);
    expect(f.held).toHaveLength(2);
    f = setPaused(f, false);
    expect(f.sent.map((e) => e.msg)).toEqual([5, 4, 3, 2, 1, 0]);
    expect(f.held).toHaveLength(0);
  });

  it("filters by type and by outcome", () => {
    let f = followFeed(EMPTY_FEED, 7);
    f = applyPush(f, {
      ...push(7, 0, 0),
      sent: [sent(1, 10, "bsm"), sent(2, 20, "cam")],
      received: [received(1, 11, "delivered"), received(2, 12, "lost")],
    });
    expect(visibleRows({ ...f, types: ["cam"] }, "sent").map((e) => e.msg)).toEqual([2]);
    expect(visibleRows({ ...f, outcome: "lost" }, "received").map((e) => e.outcome)).toEqual(["lost"]);
    expect(visibleRows({ ...f, outcome: "delivered" }, "received").map((e) => e.outcome)).toEqual(["delivered"]);
  });

  it("lays the SPDU out in rows with every octet in its span", () => {
    const d = VECTOR.sent[0].decoded;
    const rows = hexRows(d.hex, d.spans, 8);
    const octets = rows.flatMap((r) => r.bytes);
    expect(octets).toHaveLength(d.spdu_bytes ?? -1);
    for (const b of octets) {
      const span = d.spans?.[b.span];
      expect(span, `octet ${b.offset} has a span`).toBeDefined();
      expect(b.offset >= (span?.start ?? -1) && b.offset < (span?.end ?? -1)).toBe(true);
    }
    const payload = d.spans?.findIndex((s) => s.layer === "payload") ?? -1;
    expect(octets.filter((b) => b.span === payload)).toHaveLength(VECTOR.sent[0].bytes.payload ?? -1);
    expect(hexRows("zz", d.spans)).toEqual([]);
  });
});

describe("an opened message stays open (the expand race)", () => {
  beforeEach(() => {
    useStudio.setState({ feed: EMPTY_FEED, selectedNode: null, selectedActor: null });
  });

  it("keeps its fields through the pushes that move its row out of view and out of the ring", () => {
    const store = useStudio.getState();
    store.setSelection(12, 7);
    useStudio.getState().applyFeed(push(7, 0, 10));
    const key = keyOf("sent", sent(3, 300_000_000));
    useStudio.getState().openFeedMessage("sent", key);
    // Twenty seconds of 10 Hz BSMs at four pushes a second: the row leaves the first twelve after
    // one push and the ring long before the end.
    for (let i = 0; i < 80; i++) useStudio.getState().applyFeed(push(7, 10 + i * 3, 3));
    const f = useStudio.getState().feed;
    expect(f.sent.some((e) => keyOf("sent", e) === key)).toBe(false);
    expect(f.open?.key).toBe(key);
    expect(f.open?.entry.msg).toBe(3);
    expect(f.open?.entry.decoded?.message?.fields.length).toBeGreaterThan(10);
  });

  it("survives the second selection a click makes, and is dropped only for another node", () => {
    useStudio.getState().setSelection(12, 7);
    useStudio.getState().applyFeed(push(7, 0, 4));
    useStudio.getState().openFeedMessage("sent", keyOf("sent", sent(1, 100_000_000)));
    // The page selects before `view.follow` answers and again after; the same node keeps its feed.
    useStudio.getState().setSelection(12, 7);
    expect(useStudio.getState().feed.open?.entry.msg).toBe(1);
    expect(useStudio.getState().feed.sent).toHaveLength(4);
    useStudio.getState().setSelection(13, 8);
    expect(useStudio.getState().feed.open).toBeNull();
    expect(useStudio.getState().feed.sent).toHaveLength(0);
    expect(useStudio.getState().feed.node).toBe(8);
  });

  it("opening a key that is not in the table changes nothing", () => {
    const f = applyPush(followFeed(EMPTY_FEED, 7), push(7, 0, 2));
    expect(openMessage(f, "sent", "tx-999")).toBe(f);
  });
});

describe("the engine's node.feed handler", () => {
  it("refuses a push it cannot read, and one for a node it is not following", () => {
    const engine = new StudioEngine();
    useStudio.setState({ feed: followFeed(EMPTY_FEED, 7), logs: [] });
    engine.handleFeed({ ...VECTOR, v: 99 });
    engine.handleFeed({ ...VECTOR, node: 7 }); // nothing is followed by this engine
    expect(useStudio.getState().feed.pushes).toBe(0);
    expect(useStudio.getState().logs.some((l) => l.message.includes("feed version 99"))).toBe(true);
  });
});
