/**
 * `node.feed` — the followed node's messages and queues (vwp-v1 §6.7 `view.follow {feed}`, §6.14).
 *
 * Added in VWP v1.1 as an additive change (§8.4): a new optional `view.follow` parameter and a new
 * notification. While a node is followed with `feed` set, the server pushes, at most `hz` times a
 * second, what the node put on the air and what it resolved since the previous push — newest first,
 * bounded, with the number left out — and its five queues at the stream's instant.
 *
 * Every decoded value is read from the frame's own octets on the server (`crates/v2xw-server/src/
 * feed/decode.rs`): the IEEE 1609.2 envelope with `v2xw-sec`'s parser, the payload with `v2xw-msg`'s
 * J2735 or ETSI decoder. A field the octets carry as "unavailable" arrives with `v: null, na: true`.
 *
 * {@link NODE_FEED_VERSION} is the feed's own schema version, carried in every push as `v`. It equals
 * the Rust `v2xw_server::feed::FEED_VERSION`, and both packages' conformance tests read the one vector
 * `docs/protocol/vectors/node-feed-v1.json`.
 */

import type { NodeId, SimTimeNs } from "./rpc.js";

/** The `node.feed` schema version this package reads. */
export const NODE_FEED_VERSION = 1;

/** `view.follow`'s `feed` option. `true` takes every default. */
export interface FeedOptions {
  /** Newest sent frames per push (0–200, default 20). */
  sent?: number;
  /** Newest receptions per push (0–500, default 40). */
  received?: number;
  /** Waiting entries listed per queue (0–100, default 8). */
  waiting?: number;
  /** Whether each entry carries its SPDU in hex (default true). */
  bytes?: boolean;
  /** Most pushes per second (0.2–20, default 4). */
  hz?: number;
}

/** What `view.follow` answers about the feed it set up. */
export interface FeedSubscription {
  v: number;
  available: boolean;
  reason?: string;
  hz?: number;
  sent?: number;
  received?: number;
  waiting?: number;
  bytes?: boolean;
}

/** One decoded field of a message. */
export interface FeedField {
  /** Stable key, e.g. `lat`, `speed`, `msg_cnt`. */
  k: string;
  /** What to print, e.g. `latitude`, `msgCnt`. */
  label: string;
  /** The value in engineering units, or text; `null` when the octets carry "unavailable". */
  v: number | string | boolean | null;
  /** The raw value the octets carried. */
  raw: unknown;
  unit?: string;
  /** True when `raw` is the element's "unavailable" sentinel. */
  na?: boolean;
}

/** A labelled byte range of the SPDU. The spans tile `[0, spdu_bytes)` in order. */
export interface FeedSpan {
  name: string;
  layer: "envelope" | "payload";
  start: number;
  end: number;
}

/** The IEEE 1609.2 envelope as parsed. */
export interface FeedSecurity {
  standard: string;
  psid: number;
  hash: string;
  generation_time_us: number | null;
  generation_location?: string;
  signer: {
    kind: "digest" | "certificate" | "self";
    hashed_id8?: string;
    certificate?: {
      type: string;
      issuer: string;
      id: Record<string, unknown>;
      craca_id: string;
      crl_series: number;
      validity_start_time32: number;
      validity_duration: string;
      app_permissions: number[];
      bytes: number | null;
    };
  };
  signature: { alg: string; r: string; s: string };
  payload_bytes: number;
}

/** A frame's octets, decoded. */
export interface FeedDecoded {
  spdu_bytes?: number;
  /** Lower-case hex of the whole SPDU, when the subscription asked for bytes. */
  hex?: string;
  spans?: FeedSpan[];
  security?: FeedSecurity;
  message?: { format: string; fields: FeedField[]; error?: string; note?: string };
  /** Why there is nothing to decode (a frame the engine sized from a table). */
  note?: string;
  /** Why the octets did not parse. */
  error?: string;
}

/** One frame the node put on the air. */
export interface FeedSent {
  msg: number;
  t_ns: SimTimeNs;
  type: string;
  bytes: {
    on_wire: number;
    payload: number | null;
    envelope: number | null;
    certificate: number | null;
    network: number | null;
    link: number | null;
  };
  radio: { power_dbm: number | null; channel: number | null; airtime_us: number | null };
  signer: "certificate" | "digest" | "self" | null;
  pseudonym: string | null;
  timing: {
    generated_ns: SimTimeNs | null;
    sign_start_ns: SimTimeNs | null;
    signed_ns: SimTimeNs | null;
    on_air_ns: SimTimeNs;
    sign_queue_ms: number | null;
    sign_ms: number | null;
    channel_access_ms: number | null;
  };
  decoded: FeedDecoded;
}

/** One reception attempt at the node, at its fate. */
export interface FeedReceived {
  msg: number | null;
  t_ns: SimTimeNs;
  type: string;
  outcome: "delivered" | "lost" | "in-flight";
  cause: string | null;
  verification: "verified" | "unverified" | null;
  rssi_dbm: number | null;
  sinr_db: number | null;
  bytes_on_wire: number | null;
  e2e_ms: number | null;
  stages_ms: Record<string, number>;
  /** Ground truth: absent on a `node`-profile connection (§5.2). */
  from?: NodeId | null;
  /** Ground truth: absent on a `node`-profile connection (§5.2). */
  dist_m?: number | null;
  /** Present when delivered: the octets the receiver verified. */
  decoded?: FeedDecoded;
}

/** One message that waited in a queue during the last step. */
export interface FeedWaiting {
  msg: number | null;
  type: string;
  from: NodeId | null;
  enqueued_ns: SimTimeNs;
  /** When it left the queue; `null` while it is still waiting at the push's instant. */
  left_ns: SimTimeNs | null;
  /** How long it waited, or has waited so far. */
  waited_ms: number;
  stage: string;
}

/** One of the node's five queues at the stream's instant. */
export interface FeedQueue {
  id: "rx" | "verify" | "app" | "tx" | "crl";
  label: string;
  what: string;
  /** Messages waiting at the push's instant; `null` for the CRL queue, whose tasks are not stamped. */
  depth: number | null;
  /**
   * The most messages waiting at once during the last step. The stream is shown on the step grid
   * and a vehicle's traffic is periodic at the same period, so the instant alone can read "empty"
   * for a queue that is busy every step; the peak and `waiting` cover the whole step.
   */
  peak: number | null;
  in_service: number;
  served: number;
  wait_p50_ms: number | null;
  wait_p95_ms: number | null;
  drops: Record<string, number>;
  drops_note?: string;
  /** Every message that waited during the last step: still waiting first, then those that left. */
  waiting: FeedWaiting[];
  waiting_omitted: number;
  /** The node's own telemetry window's depth percentiles. */
  reported_depth: { p50: number | null; p95: number | null } | null;
}

/** The node's queues. */
export interface FeedQueues {
  t_ns: SimTimeNs;
  /** The step a reading covers (`peak`, `waiting`). */
  step_ms: number;
  /**
   * How far past the push's instant the engine had simulated. A message still queued at the
   * instant is known only if the engine has run past the moment it leaves, so a lead of a step or
   * two (a kernel slower than real time) can under-count `depth`.
   */
  kernel_lead_ms?: number;
  window_ms: number;
  drop_window_ms: number;
  source: string;
  list: FeedQueue[];
}

/** One `node.feed` push. */
export interface NodeFeedNotification {
  v: number;
  node: NodeId;
  t_ns: SimTimeNs;
  /** The instant the previous push covered; `null` on the first push and after a reset. */
  since_ns: SimTimeNs | null;
  /** True when this push starts over (the first one, or after a backward seek). */
  reset: boolean;
  /** Newest first. */
  sent: FeedSent[];
  /** Newest first. */
  received: FeedReceived[];
  /** How many new entries this push left out. */
  omitted: { sent: number; received: number };
  /** Entries the server's per-node cap shed since the run began. */
  shed: { sent: number; received: number };
  /** Attempts the receiver never detected (out of range, below sensitivity): counted, not listed. */
  undetected: number;
  /** How far behind the stream the server keeps a node's traffic. */
  history_ns: number;
  queues: FeedQueues;
}

const isObj = (x: unknown): x is Record<string, unknown> => typeof x === "object" && x !== null && !Array.isArray(x);
const isNum = (x: unknown): x is number => typeof x === "number" && Number.isFinite(x);

/**
 * Checks a `node.feed` push before anything renders it.
 *
 * Returns the push, or a reason it was refused: a different schema version, or a required member
 * missing or of the wrong type. It checks structure, not every leaf — the Studio prints what it is
 * given — but it does check every member a table row is built from.
 */
export function parseNodeFeed(raw: unknown): { ok: true; feed: NodeFeedNotification } | { ok: false; reason: string } {
  if (!isObj(raw)) return { ok: false, reason: "not an object" };
  if (raw.v !== NODE_FEED_VERSION) return { ok: false, reason: `feed version ${String(raw.v)}, this client reads ${NODE_FEED_VERSION}` };
  for (const k of ["node", "t_ns", "undetected", "history_ns"]) {
    if (!isNum(raw[k])) return { ok: false, reason: `${k} is not a number` };
  }
  if (typeof raw.reset !== "boolean") return { ok: false, reason: "reset is not a boolean" };
  if (!Array.isArray(raw.sent) || !Array.isArray(raw.received)) return { ok: false, reason: "sent/received are not arrays" };
  for (const s of raw.sent) {
    if (!isObj(s) || !isNum(s.msg) || !isNum(s.t_ns) || typeof s.type !== "string" || !isObj(s.bytes) || !isObj(s.decoded)) {
      return { ok: false, reason: "a sent entry lacks msg, t_ns, type, bytes or decoded" };
    }
  }
  for (const r of raw.received) {
    if (!isObj(r) || !isNum(r.t_ns) || typeof r.outcome !== "string" || typeof r.type !== "string") {
      return { ok: false, reason: "a received entry lacks t_ns, type or outcome" };
    }
  }
  if (!isObj(raw.omitted) || !isNum(raw.omitted.sent) || !isNum(raw.omitted.received)) return { ok: false, reason: "omitted is malformed" };
  if (!isObj(raw.queues) || !Array.isArray(raw.queues.list)) return { ok: false, reason: "queues.list is not an array" };
  for (const q of raw.queues.list) {
    if (!isObj(q) || typeof q.id !== "string" || !Array.isArray(q.waiting) || !isObj(q.drops)) {
      return { ok: false, reason: "a queue lacks id, waiting or drops" };
    }
  }
  return { ok: true, feed: raw as unknown as NodeFeedNotification };
}
