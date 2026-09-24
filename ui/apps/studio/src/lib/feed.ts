/**
 * The followed node's message feed, as the message panel shows it.
 *
 * The engine pushes `node.feed` a few times a second while a vehicle is followed (vwp-v1 §6.7,
 * §6.14): the frames it put on the air, the receptions it resolved, and its five queues. This module
 * keeps them — a ring of the newest {@link RING} per direction — and answers what the panel asks:
 * which rows pass the filters, which message is open, and how its octets lay out in hex.
 *
 * # The open message is pinned
 *
 * Opening a row used to keep only the row's key, and the detail was drawn from whichever row had that
 * key in the newest refresh's first twelve. A BSM stream at 10 Hz pushed the opened row out of those
 * twelve one to two seconds after the click, depending on where in the once-a-second refresh the click
 * fell — so the same click showed its fields in one pass and nothing in the next. Here the opened
 * message is a copy of the entry itself: new pushes, the ring dropping it, a filter hiding its row or
 * the panel re-mounting do not take it away. Only choosing another message, or following another
 * node, does.
 *
 * # Pause
 *
 * Pausing freezes what is shown and keeps receiving: pushes are held, counted, and applied on resume,
 * so nothing is lost by pausing and the table does not move under the pointer.
 *
 * Framework- and DOM-free, so it is tested in plain Node (test/feed.test.ts).
 */

import type { FeedQueues, FeedReceived, FeedSent, FeedSpan, NodeFeedNotification } from "@vwp/protocol";

/** Newest entries kept per direction. */
export const RING = 200;

/** A row's direction. */
export type FeedDir = "sent" | "received";

/** The pinned, open message. */
export interface OpenMessage {
  readonly dir: FeedDir;
  readonly key: string;
  readonly entry: FeedSent | FeedReceived;
}

/** What the panel shows. */
export interface FeedView {
  /** The node these rows belong to; a push for any other node is ignored. */
  readonly node: number | null;
  /** Newest first. */
  readonly sent: readonly FeedSent[];
  /** Newest first. */
  readonly received: readonly FeedReceived[];
  readonly queues: FeedQueues | null;
  /** The stream instant of the newest push applied. */
  readonly tNs: number;
  /** New entries the pushes left out (the server's per-push bound), since following began. */
  readonly omitted: { readonly sent: number; readonly received: number };
  /** Entries this page's ring dropped, since following began. */
  readonly dropped: { readonly sent: number; readonly received: number };
  /** Attempts the receiver never detected, as the server counts them. */
  readonly undetected: number;
  readonly paused: boolean;
  /** Pushes received while paused, applied on resume. */
  readonly held: readonly NodeFeedNotification[];
  readonly open: OpenMessage | null;
  /** Message types to show; empty shows every type. */
  readonly types: readonly string[];
  /** Which receptions to show. */
  readonly outcome: "all" | "delivered" | "lost";
  /** How many pushes have been applied, for a "live" indicator. */
  readonly pushes: number;
  /** Why the feed is unavailable, when the engine said so. */
  readonly unavailable: string | null;
}

export const EMPTY_FEED: FeedView = {
  node: null,
  sent: [],
  received: [],
  queues: null,
  tNs: 0,
  omitted: { sent: 0, received: 0 },
  dropped: { sent: 0, received: 0 },
  undetected: 0,
  paused: false,
  held: [],
  open: null,
  types: [],
  outcome: "all",
  pushes: 0,
  unavailable: null,
};

/** A stable key for a row: the message id, and for a reception its instant (one message is heard once per receiver). */
export function keyOf(dir: FeedDir, e: FeedSent | FeedReceived): string {
  return dir === "sent" ? `tx-${String(e.msg)}` : `rx-${String(e.msg)}-${e.t_ns}`;
}

/** The feed for a newly followed node: empty, filters kept. */
export function followFeed(state: FeedView, node: number | null): FeedView {
  return { ...EMPTY_FEED, node, types: state.types, outcome: state.outcome };
}

/** Merge newest-first `fresh` over newest-first `kept`, dropping duplicates, bounded at `ring`. */
function merge<T extends FeedSent | FeedReceived>(
  dir: FeedDir,
  fresh: readonly T[],
  kept: readonly T[],
  ring: number,
): { rows: T[]; dropped: number } {
  const seen = new Set<string>();
  const rows: T[] = [];
  for (const e of [...fresh, ...kept]) {
    const k = keyOf(dir, e);
    if (seen.has(k)) continue;
    seen.add(k);
    rows.push(e);
  }
  const dropped = Math.max(0, rows.length - ring);
  return { rows: rows.slice(0, ring), dropped };
}

/** Apply one push. A push for another node, or of another schema version, changes nothing. */
export function applyPush(state: FeedView, push: NodeFeedNotification, ring = RING): FeedView {
  if (state.node === null || push.node !== state.node) return state;
  if (state.paused) return { ...state, held: [...state.held, push].slice(-64) };
  const base = push.reset ? { ...state, sent: [], received: [] } : state;
  const s = merge("sent", push.sent, base.sent, ring);
  const r = merge("received", push.received, base.received, ring);
  return {
    ...base,
    sent: s.rows,
    received: r.rows,
    queues: push.queues,
    tNs: push.t_ns,
    omitted: {
      sent: base.omitted.sent + push.omitted.sent,
      received: base.omitted.received + push.omitted.received,
    },
    dropped: { sent: base.dropped.sent + s.dropped, received: base.dropped.received + r.dropped },
    undetected: push.undetected,
    pushes: base.pushes + 1,
    unavailable: null,
  };
}

/** Pause or resume. Resuming applies every held push in order. */
export function setPaused(state: FeedView, paused: boolean): FeedView {
  if (paused) return { ...state, paused: true };
  let next: FeedView = { ...state, paused: false, held: [] };
  for (const p of state.held) next = applyPush(next, p);
  return next;
}

/** Open a message (or close it, with `null`). The entry is copied in: it stays open whatever arrives next. */
export function openMessage(state: FeedView, dir: FeedDir, key: string | null): FeedView {
  if (key === null) return { ...state, open: null };
  const rows: readonly (FeedSent | FeedReceived)[] = dir === "sent" ? state.sent : state.received;
  const entry = rows.find((e) => keyOf(dir, e) === key);
  if (!entry) return state;
  return { ...state, open: { dir, key, entry } };
}

/** The rows of one direction that pass the filters. */
export function visibleRows<D extends FeedDir>(state: FeedView, dir: D): D extends "sent" ? FeedSent[] : FeedReceived[] {
  const types = new Set(state.types);
  if (dir === "sent") {
    return state.sent.filter((e) => types.size === 0 || types.has(e.type)) as never;
  }
  return state.received.filter(
    (e) =>
      (types.size === 0 || types.has(e.type)) &&
      (state.outcome === "all" || (state.outcome === "delivered" ? e.outcome === "delivered" : e.outcome !== "delivered")),
  ) as never;
}

/** Every message type seen in either direction, sorted: the type filter's options. */
export function typesSeen(state: FeedView): string[] {
  const s = new Set<string>();
  for (const e of state.sent) s.add(e.type);
  for (const e of state.received) s.add(e.type);
  return [...s].sort();
}

/** One byte of a hex dump, with the span it belongs to. */
export interface HexByte {
  readonly offset: number;
  readonly hex: string;
  /** Index into the spans, or -1. */
  readonly span: number;
}

/** One line of a hex dump. */
export interface HexRow {
  readonly offset: number;
  readonly bytes: readonly HexByte[];
}

/**
 * Lay the SPDU's octets out in rows of `perRow`, each byte tagged with the span it falls in.
 *
 * Returns no rows for hex that is not whole octets of hex digits, rather than a dump of garbage.
 */
export function hexRows(hex: string | undefined, spans: readonly FeedSpan[] | undefined, perRow = 8): HexRow[] {
  if (!hex || hex.length % 2 !== 0 || !/^[0-9a-f]*$/i.test(hex)) return [];
  const n = hex.length / 2;
  const spanOf = new Int32Array(n).fill(-1);
  (spans ?? []).forEach((s, i) => {
    for (let b = Math.max(0, s.start); b < Math.min(n, s.end); b++) spanOf[b] = i;
  });
  const rows: HexRow[] = [];
  for (let o = 0; o < n; o += perRow) {
    const bytes: HexByte[] = [];
    for (let b = o; b < Math.min(n, o + perRow); b++) {
      bytes.push({ offset: b, hex: hex.slice(2 * b, 2 * b + 2), span: spanOf[b] });
    }
    rows.push({ offset: o, bytes });
  }
  return rows;
}

/** A decoded field's value as a string, with its unit; "unavailable" when the octets say so. */
export function fieldText(v: number | string | boolean | null, unit: string | undefined, na: boolean | undefined): string {
  if (na) return "unavailable";
  if (v === null) return "—";
  return unit ? `${String(v)} ${unit}` : String(v);
}

/** The decoded field `k` of an entry, or undefined. */
export function fieldOf(e: FeedSent | FeedReceived, k: string): { v: number | string | boolean | null; unit?: string; na?: boolean } | undefined {
  return e.decoded?.message?.fields.find((f) => f.k === k);
}

/** Degrees from north, for an ENU heading in radians (0 = east, counter-clockwise). */
export function bearingDeg(headingRad: number): number {
  const d = 90 - (headingRad * 180) / Math.PI;
  return ((d % 360) + 360) % 360;
}
