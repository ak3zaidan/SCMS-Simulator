/**
 * Unit formatting for the HUD and inspector.
 *
 * Every function here knows the wire unit of the field it formats, taken from the tables in
 * docs/protocol/vwp-v1.md §3.5.2 (per-mille, centi-dBm, KiB, ns, …), and it honours the
 * "unknown/not-modelled" sentinels of §3.5.2: `0xFFFF` for `u16`, `0xFFFF_FFFF` for `u32`,
 * `u64::MAX` for `u64` times and `NaN` for `f32`. A sentinel renders as `n/a`, never as a number —
 * showing 65,535 neighbours because the tier does not model a neighbour table would be a lie.
 */

import { SENTINEL_U8, SENTINEL_U16, SENTINEL_U32, SENTINEL_U64 } from "@vwp/protocol";

/** What the UI prints where the engine said "not modelled at this tier". */
export const NA = "n/a";

/** §3.5.2 — `u16` at `0xFFFF` means unknown; at `0xFFFE`… it is a real value. */
export function isU16Sentinel(v: number): boolean {
  return v === SENTINEL_U16;
}

/** §3.5.2 — `u32` at `0xFFFF_FFFF` means unknown. */
export function isU32Sentinel(v: number): boolean {
  return v === SENTINEL_U32;
}

/** §3.5.2 — `u64` at `u64::MAX` means "none"/unknown (e.g. `next_topup_ns`). */
export function isU64Sentinel(v: bigint): boolean {
  return v === SENTINEL_U64;
}

/** §3.5.2 — `u8` at `0xFF`. */
export function isU8Sentinel(v: number): boolean {
  return v === SENTINEL_U8;
}

/** §3.5.2 — "any `u16` counter at its maximum means ≥ 65535", which is a value, not a sentinel. */
function u16(v: number, render: (n: number) => string): string {
  return isU16Sentinel(v) ? NA : render(v);
}

/** Group digits with thin separators: 1040 → "1,040". */
export function int(v: number): string {
  if (!Number.isFinite(v)) return NA;
  return Math.round(v).toLocaleString("en-US");
}

/** A `u32` count, honouring the §3.5.2 sentinel. */
export function countU32(v: number): string {
  return isU32Sentinel(v) ? NA : int(v);
}

/** A `u16` count, honouring the §3.5.2 sentinel. */
export function countU16(v: number): string {
  return u16(v, int);
}

/** Fixed-precision number; `NaN` (the §3.5.2 f32 sentinel) renders as `n/a`. */
export function num(v: number, digits = 1): string {
  return Number.isFinite(v) ? v.toFixed(digits) : NA;
}

/** Per-mille (`*_pm`) as a percentage: 612 → "61.2 %". */
export function permillePct(v: number, digits = 1): string {
  return u16(v, (n) => `${(n / 10).toFixed(digits)} %`);
}

/** Per-mille as a 0–1 ratio: 420 → "0.420". */
export function permilleRatio(v: number, digits = 3): string {
  return u16(v, (n) => (n / 1000).toFixed(digits));
}

/** Per-mille as a number in `[0, 1]`, or `null` at the sentinel — for plotting. */
export function permilleValue(v: number): number | null {
  return isU16Sentinel(v) ? null : v / 1000;
}

/** centi-dBm (`tx_power_cdbm`) → dBm: 1700 → "17.0 dBm". */
export function cdbm(v: number): string {
  return v === -32768 ? NA : `${(v / 100).toFixed(1)} dBm`;
}

/** KiB (`ram_used_kib`) rendered in the largest sensible binary unit. */
export function kib(v: number): string {
  return isU32Sentinel(v) ? NA : bytes(v * 1024);
}

/** Bytes in binary units. Accepts the `u64` fields as `bigint`. */
export function bytes(v: number | bigint): string {
  if (typeof v === "bigint") {
    if (isU64Sentinel(v)) return NA;
    return bytes(Number(v));
  }
  if (!Number.isFinite(v)) return NA;
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let n = v;
  let i = 0;
  while (n >= 1024 && i < units.length - 1) {
    n /= 1024;
    i++;
  }
  return `${n.toFixed(i === 0 ? 0 : n >= 100 ? 0 : 1)} ${units[i]}`;
}

/** A duration in nanoseconds as `1d 03h`, `4m 12s`, `840 ms`. */
export function durationNs(ns: number | bigint): string {
  const n = typeof ns === "bigint" ? (isU64Sentinel(ns) ? Number.NaN : Number(ns)) : ns;
  if (!Number.isFinite(n)) return NA;
  const seconds = n / 1e9;
  if (Math.abs(seconds) < 1) return `${(n / 1e6).toFixed(0)} ms`;
  const abs = Math.abs(seconds);
  const sign = seconds < 0 ? "−" : "";
  if (abs < 60) return `${sign}${abs.toFixed(1)} s`;
  const d = Math.floor(abs / 86400);
  const h = Math.floor((abs % 86400) / 3600);
  const m = Math.floor((abs % 3600) / 60);
  const s = Math.floor(abs % 60);
  if (d > 0) return `${sign}${d}d ${String(h).padStart(2, "0")}h`;
  if (h > 0) return `${sign}${h}h ${String(m).padStart(2, "0")}m`;
  return `${sign}${m}m ${String(s).padStart(2, "0")}s`;
}

/** Simulated time as `hh:mm:ss.mmm`, the form the scrub bar and event markers use. */
export function simClock(ns: number | bigint): string {
  const n = typeof ns === "bigint" ? Number(ns) : ns;
  if (!Number.isFinite(n)) return NA;
  const total = n / 1e9;
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = Math.floor(total % 60);
  const ms = Math.floor((total % 1) * 1000);
  return `${String(h).padStart(2, "0")}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}.${String(ms).padStart(3, "0")}`;
}

/** A signed nanosecond offset (`clock_offset_ns`) in the largest readable unit. */
export function signedNs(ns: bigint): string {
  if (isU64Sentinel(ns)) return NA;
  const n = Number(ns);
  const sign = n < 0 ? "−" : "+";
  const a = Math.abs(n);
  if (a < 1000) return `${sign}${a} ns`;
  if (a < 1e6) return `${sign}${(a / 1000).toFixed(1)} µs`;
  if (a < 1e9) return `${sign}${(a / 1e6).toFixed(2)} ms`;
  return `${sign}${(a / 1e9).toFixed(3)} s`;
}

/** First 4 and last 2 bytes of a digest, the form 09-ui §5 shows: `7f3a…9c`. */
export function shortDigest(hex: string): string {
  if (hex.length <= 8) return hex;
  return `${hex.slice(0, 4)}…${hex.slice(-2)}`;
}

/** Lower-case hex of a byte array. */
export function hex(bytesIn: Uint8Array): string {
  let out = "";
  for (let i = 0; i < bytesIn.length; i++) out += bytesIn[i].toString(16).padStart(2, "0");
  return out;
}

/** §3.5.2 — `dcc_state`: 0 RELAXED … 5 C-V2X congestion control, `0xFFFF` n/a. */
export const DCC_STATES = ["RELAXED", "ACTIVE_1", "ACTIVE_2", "ACTIVE_3", "RESTRICTIVE", "CV2X_CC"] as const;
export function dccState(v: number): string {
  if (isU16Sentinel(v)) return NA;
  return DCC_STATES[v] ?? `state ${v}`;
}

/** §3.5.2 — `gnss_fix`: 0 none … 6 dead-reckoning. */
export const GNSS_FIXES = ["none", "2D", "3D", "DGNSS", "RTK-float", "RTK-fix", "dead-reckoning"] as const;
export function gnssFix(v: number): string {
  if (isU8Sentinel(v)) return NA;
  return GNSS_FIXES[v] ?? `fix ${v}`;
}

/** §3.5.2 — `node_state`: 0 off … 6 compromised (6 is ground truth). */
export const NODE_STATES = ["off", "booting", "active", "parked", "degraded", "down", "compromised"] as const;
export function nodeState(v: number): string {
  if (isU8Sentinel(v)) return NA;
  return NODE_STATES[v] ?? `state ${v}`;
}

/** §3.5.2 — `verify_policy`: 0 verify-all, 1 on-demand, 2 prioritized. */
export const VERIFY_POLICIES = ["verify-all", "on-demand", "prioritized"] as const;
export function verifyPolicy(v: number): string {
  if (isU8Sentinel(v)) return NA;
  return VERIFY_POLICIES[v] ?? `policy ${v}`;
}

/** §3.1.3 — node `kind`. */
export const NODE_KINDS = ["obu", "vru-device", "rsu", "base-station", "router", "backend-entity", "other"] as const;

/** §3.8 — `subject_kind` of a provenance entry. */
export const PROV_SUBJECT_KINDS = ["value", "node", "link", "actor", "metric", "channel", "world"] as const;
