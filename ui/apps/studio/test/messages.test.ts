/**
 * The followed vehicle's message log: newest first, content in one line, octets that add up, and
 * the frame at which the pseudonym changed marked as such.
 */

import { describe, expect, it } from "vitest";
import type { InspectReceivedMessage, InspectSentMessage } from "@vwp/protocol";

import { receivedRows, sentRows } from "../src/lib/messages.js";

function bsm(t_ns: number, count: number, pseudonym: string): InspectSentMessage {
  return {
    t_ns,
    msg: count,
    msg_type: "bsm",
    bytes_on_wire: 193,
    payload_bytes: 40,
    envelope_bytes: 110,
    cert_bytes: 0,
    net_header_bytes: 5,
    link_bytes: 38,
    airtime_us: 312,
    power_dbm: 20,
    signer: "digest",
    t_generated_ns: t_ns - 12_000_000,
    t_signed_ns: t_ns - 3_000_000,
    channel: 172,
    pseudonym,
    content: {
      msg_count: count,
      temp_id: pseudonym.slice(0, 8),
      sec_mark_ms: 1234,
      lat_deg: 40.7581234,
      lon_deg: -73.9854321,
      speed_mps: 8.34,
      heading_deg: 271.4,
      part_ii: 0,
    },
  };
}

describe("sentRows", () => {
  it("lists the newest broadcast first with its decoded content", () => {
    const rows = sentRows([bsm(1e9, 1, "aaaaaaaa11111111"), bsm(1.1e9, 2, "aaaaaaaa11111111")]);
    expect(rows.map((r) => r.summary)).toEqual([
      "#2 · id aaaaaaaa · 40.75812°, -73.98543° · 8.3 m/s · 271°",
      "#1 · id aaaaaaaa · 40.75812°, -73.98543° · 8.3 m/s · 271°",
    ]);
    expect(rows[0].type).toBe("BSM");
    expect(rows[0].layers).toBe("payload 40 + security 110 + network 5 + link 38 = 193 B");
    expect(rows[0].signMs).toBeCloseTo(9, 9);
    expect(rows[0].fields).toContainEqual(["temporary id", "aaaaaaaa"]);
  });

  it("marks the frame on which the pseudonym changed, and only that one", () => {
    const rows = sentRows([
      bsm(1e9, 1, "aaaaaaaa11111111"),
      bsm(1.1e9, 2, "aaaaaaaa11111111"),
      bsm(1.2e9, 3, "bbbbbbbb22222222"),
      bsm(1.3e9, 4, "bbbbbbbb22222222"),
    ]);
    // Newest first: #4, #3 (the rotation), #2, #1.
    expect(rows.map((r) => r.newPseudonym)).toEqual([false, true, false, false]);
  });

  it("says nothing about content a frame did not carry", () => {
    const rows = sentRows([{ t_ns: 5e8, bytes_on_wire: 300, msg_type: "denm" }]);
    expect(rows[0].summary).toBe("n/a");
    expect(rows[0].layers).toBe("300 B");
    expect(rows[0].newPseudonym).toBe(false);
  });
});

describe("receivedRows", () => {
  it("names the fate of each reception, newest first", () => {
    const received: InspectReceivedMessage[] = [
      { t_ns: 1e9, from: 3, msg_type: "bsm", outcome: "delivered", verification: "verified", rssi_dbm: -71.24, sinr_db: 20.1, dist_m: 84.4, e2e_ms: 14.567 },
      { t_ns: 1.05e9, from: 4, msg_type: "bsm", outcome: "lost", cause: "collision", rssi_dbm: -88, sinr_db: 1.2, dist_m: 210 },
    ];
    const rows = receivedRows(received);
    expect(rows.map((r) => r.fate)).toEqual(["lost · collision", "delivered · verified"]);
    expect(rows[1].e2e).toBe("14.57 ms");
    expect(rows[1].signal).toBe("-71.2 dBm · SINR 20.1 dB");
    expect(rows[0].e2e).toBe("n/a");
  });
});
