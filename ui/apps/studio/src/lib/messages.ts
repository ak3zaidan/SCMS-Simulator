/**
 * The followed vehicle's message log, as rows a panel can print.
 *
 * `inspect.node`'s `messages` section (crates/v2xw-server `sent_json` / `received_json`) is the
 * node's recent traffic, oldest first: every frame it broadcast, with the pseudonym that signed it
 * and what the message said, and every reception at it followed to its fate. These functions turn
 * that into newest-first rows with their units, and mark the frame at which the pseudonym changed,
 * so a rotation is something one can see happen on the air rather than a counter.
 *
 * Framework- and DOM-free, so it is tested in plain Node (test/messages.test.ts).
 */

import type { InspectMessageContent, InspectReceivedMessage, InspectSentMessage } from "@vwp/protocol";

import { NA, shortDigest, simClock } from "./format.js";

/** One broadcast frame, ready to print. */
export interface SentRow {
  readonly key: string;
  readonly time: string;
  readonly type: string;
  /** The message's own fields in one line: count, temporary id, position, speed, heading. */
  readonly summary: string;
  /** Octets by layer, summing to the frame on the air. */
  readonly layers: string;
  readonly totalBytes: number;
  /** `certificate` when the full certificate was attached, `digest` otherwise. */
  readonly signer: string;
  readonly pseudonym: string | null;
  /** True on the first frame signed with a pseudonym different from the frame before it. */
  readonly newPseudonym: boolean;
  /** Power, channel and air time. */
  readonly radio: string;
  /** Generation to the end of signing, ms: how long the message waited for and in the signer. */
  readonly signMs: number | null;
  /** Every decoded field, label and value, for the expanded view. */
  readonly fields: readonly (readonly [string, string])[];
}

/** One reception at the followed node, ready to print. */
export interface ReceivedRow {
  readonly key: string;
  readonly time: string;
  readonly from: string;
  readonly type: string;
  readonly delivered: boolean;
  /** `delivered · verified`, `lost · collision`, `in flight`. */
  readonly fate: string;
  readonly signal: string;
  readonly distance: string;
  readonly e2e: string;
}

const fixed = (v: number | null | undefined, digits: number, unit = ""): string =>
  typeof v === "number" && Number.isFinite(v) ? `${v.toFixed(digits)}${unit}` : NA;

function contentFields(c: InspectMessageContent | null | undefined): [string, string][] {
  if (!c) return [];
  const out: [string, string][] = [];
  if (c.msg_count !== undefined) out.push(["msgCnt", String(c.msg_count)]);
  if (c.temp_id !== undefined) out.push(["temporary id", c.temp_id]);
  if (c.sec_mark_ms !== undefined) out.push(["secMark", `${c.sec_mark_ms} ms`]);
  if (c.lat_deg !== undefined) out.push(["latitude", fixed(c.lat_deg, 7, "°")]);
  if (c.lon_deg !== undefined) out.push(["longitude", fixed(c.lon_deg, 7, "°")]);
  if (c.elev_m !== undefined) out.push(["elevation", fixed(c.elev_m, 1, " m")]);
  if (c.speed_mps !== undefined) out.push(["speed", fixed(c.speed_mps, 2, " m/s")]);
  if (c.heading_deg !== undefined) out.push(["heading", fixed(c.heading_deg, 2, "° from north")]);
  if (c.part_ii !== undefined) out.push(["Part II containers", String(c.part_ii)]);
  if (c.claimed_x_m !== undefined && c.claimed_y_m !== undefined) {
    out.push(["claimed position", `${fixed(c.claimed_x_m, 1)} m E, ${fixed(c.claimed_y_m, 1)} m N`]);
  }
  if (c.claimed_speed_mps !== undefined) out.push(["claimed speed", fixed(c.claimed_speed_mps, 2, " m/s")]);
  return out;
}

function summaryOf(c: InspectMessageContent | null | undefined): string {
  if (!c) return NA;
  const parts: string[] = [];
  if (c.msg_count !== undefined) parts.push(`#${c.msg_count}`);
  if (c.temp_id !== undefined) parts.push(`id ${c.temp_id}`);
  if (c.lat_deg !== undefined && c.lon_deg !== undefined) parts.push(`${fixed(c.lat_deg, 5, "°")}, ${fixed(c.lon_deg, 5, "°")}`);
  if (c.speed_mps !== undefined) parts.push(fixed(c.speed_mps, 1, " m/s"));
  if (c.heading_deg !== undefined) parts.push(fixed(c.heading_deg, 0, "°"));
  return parts.length > 0 ? parts.join(" · ") : NA;
}

function layersOf(m: InspectSentMessage): string {
  const parts: string[] = [];
  const add = (label: string, v: number | null | undefined): void => {
    if (typeof v === "number") parts.push(`${label} ${v}`);
  };
  add("payload", m.payload_bytes);
  if (typeof m.envelope_bytes === "number") {
    const cert = typeof m.cert_bytes === "number" && m.cert_bytes > 0 ? ` (cert ${m.cert_bytes})` : "";
    parts.push(`security ${m.envelope_bytes}${cert}`);
  }
  add("network", m.net_header_bytes);
  add("link", m.link_bytes);
  return parts.length > 0 ? `${parts.join(" + ")} = ${m.bytes_on_wire} B` : `${m.bytes_on_wire} B`;
}

/** The frames the node broadcast, newest first. */
export function sentRows(sent: readonly InspectSentMessage[] | undefined): SentRow[] {
  if (!sent) return [];
  const rows: SentRow[] = [];
  let previous: string | null = null;
  for (let i = 0; i < sent.length; i++) {
    const m = sent[i];
    const pseudonym = m.pseudonym ?? null;
    const signMs =
      typeof m.t_signed_ns === "number" && typeof m.t_generated_ns === "number"
        ? (m.t_signed_ns - m.t_generated_ns) / 1e6
        : null;
    rows.push({
      key: `${m.t_ns}-${m.msg ?? i}`,
      time: simClock(m.t_ns),
      type: (m.msg_type ?? "frame").toUpperCase(),
      summary: summaryOf(m.content),
      layers: layersOf(m),
      totalBytes: m.bytes_on_wire,
      signer: m.signer ?? NA,
      pseudonym,
      newPseudonym: previous !== null && pseudonym !== null && pseudonym !== previous,
      radio: `${fixed(m.power_dbm, 1, " dBm")} · ch ${m.channel ?? NA} · ${typeof m.airtime_us === "number" ? `${m.airtime_us} µs` : NA}`,
      signMs,
      fields: [
        ...contentFields(m.content),
        ["pseudonym", pseudonym ?? NA],
        ["signer", m.signer ?? NA],
        ["octets", layersOf(m)],
        ["generated → signed", signMs === null ? NA : `${signMs.toFixed(3)} ms`],
      ],
    });
    if (pseudonym !== null) previous = pseudonym;
  }
  return rows.reverse();
}

/** The receptions at the node, newest first. */
export function receivedRows(received: readonly InspectReceivedMessage[] | undefined): ReceivedRow[] {
  if (!received) return [];
  const rows = received.map((m, i): ReceivedRow => {
    const delivered = m.outcome === "delivered";
    const fate = delivered
      ? `delivered${m.verification ? ` · ${m.verification}` : ""}`
      : m.outcome === "in-flight"
        ? "in flight"
        : `lost${m.cause ? ` · ${m.cause}` : ""}`;
    return {
      key: `${m.t_ns}-${m.from ?? "?"}-${m.msg ?? i}`,
      time: simClock(m.t_ns),
      from: typeof m.from === "number" ? `node ${m.from}` : NA,
      type: (m.msg_type ?? "frame").toUpperCase(),
      delivered,
      fate,
      signal: `${fixed(m.rssi_dbm, 1, " dBm")} · SINR ${fixed(m.sinr_db, 1, " dB")}`,
      distance: fixed(m.dist_m, 0, " m"),
      e2e: typeof m.e2e_ms === "number" ? `${m.e2e_ms.toFixed(2)} ms` : NA,
    };
  });
  return rows.reverse();
}

/** A pseudonym in the short form the HUD uses. */
export function shortPseudonym(p: string | null): string {
  return p === null ? NA : shortDigest(p);
}
