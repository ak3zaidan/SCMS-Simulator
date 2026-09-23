/**
 * The `NodeTelemetry` record of docs/protocol/vwp-v1.md §3.5.2, turned into the HUD of 09-ui §5.
 *
 * Every one of the record's 55 fields appears here exactly once, in the group the HUD sketch puts it
 * in, with the unit from the spec table and the visibility tag from its `Vis` column. Nothing is
 * invented and nothing is silently dropped: if a field is at its §3.5.2 "not modelled" sentinel the
 * formatter prints `n/a`, and the two values the HUD sketch shows that the record does **not** carry
 * (the evidence buffer and the last-CRL-fetch time) are listed as {@link MISSING_FROM_WIRE} rather
 * than faked.
 */

import type { NodeTelemetry } from "@vwp/protocol";
import {
  NA,
  bytes,
  cdbm,
  countU16,
  countU32,
  dccState,
  durationNs,
  gnssFix,
  int,
  isU16Sentinel,
  isU32Sentinel,
  isU64Sentinel,
  kib,
  nodeState,
  num,
  permillePct,
  permilleRatio,
  signedNs,
  verifyPolicy,
} from "./format.js";

/**
 * Who is allowed to see a value.
 *
 * `GT` is ground truth: something the run knows and no radio in it could. It is withheld from a
 * detector under evaluation, and the interface tags it so a reader never mistakes it for something
 * a vehicle observed.
 */
export type FieldVisibility = "GT" | "NODE" | "PUBLIC";

/** One value on the HUD, carrying enough to explain itself in the "why" tab. */
export interface HudField {
  /** The telemetry field's own name, which is what the engine is asked about when explaining it. */
  readonly key: string;
  readonly label: string;
  /** Already formatted, sentinels resolved to `n/a`. */
  readonly value: string;
  /** The number behind the text, or `null` at a sentinel. Plots and sparklines use this. */
  readonly raw: number | null;
  readonly unit: string;
  readonly visibility: FieldVisibility;
  /** Shown under the value in the inspector. */
  readonly help?: string;
}

/** A labelled block of HUD fields. */
export interface HudGroup {
  readonly id: string;
  readonly title: string;
  readonly fields: readonly HudField[];
}

/** One queue row of the §3.5.2 `q_*_p50` / `q_*_p95` pairs. */
export interface QueueRow {
  readonly id: string;
  readonly label: string;
  readonly unit: string;
  readonly p50: number | null;
  readonly p95: number | null;
  readonly drops: readonly { key: string; label: string; value: number | null }[];
}

/**
 * Values the HUD has a place for that a radio's regular report does not carry.
 *
 * Rendered as explicit gaps with the reason, and filled from a direct question to the engine about
 * that one radio when it answers. A blank would read as zero; this reads as unknown, which is what
 * it is.
 */
export const MISSING_FROM_WIRE = [
  {
    key: "evidence_buffer",
    label: "Evidence buffer",
    reason: "The radio's regular report does not include this, so it has to be asked for separately — and not every engine answers.",
    inspectPath: ["stores", "evidence_buffer"],
  },
  {
    key: "last_crl_fetch",
    label: "Last CRL fetch",
    reason: "The radio's regular report does not include when it last fetched the revocation list, so it has to be asked for separately.",
    inspectPath: ["crl", "last_fetch_ns"],
  },
] as const;

function u16v(v: number): number | null {
  return isU16Sentinel(v) ? null : v;
}
function u32v(v: number): number | null {
  return isU32Sentinel(v) ? null : v;
}
function f32v(v: number): number | null {
  return Number.isFinite(v) ? v : null;
}

/** Percentage of `used / total`, or `null` when either side is a sentinel or zero. */
function ratioPct(used: number, total: number): number | null {
  if (!Number.isFinite(used) || !Number.isFinite(total) || total <= 0) return null;
  return (used / total) * 100;
}

/** The five queues a radio reports, with what it dropped from each and why. */
export function queueRows(t: NodeTelemetry): QueueRow[] {
  return [
    {
      id: "rx", label: "RX", unit: "messages", p50: u16v(t.qRxP50), p95: u16v(t.qRxP95),
      drops: [{ key: "drop_rx_overflow", label: "overflow", value: u32v(t.dropRxOverflow) }],
    },
    {
      id: "verify", label: "Verify", unit: "messages", p50: u16v(t.qVerifyP50), p95: u16v(t.qVerifyP95),
      drops: [
        { key: "drop_verify_overflow", label: "overflow", value: u32v(t.dropVerifyOverflow) },
        { key: "drop_verify_policy_skip", label: "policy skip", value: u32v(t.dropVerifyPolicySkip) },
      ],
    },
    { id: "app", label: "App", unit: "messages", p50: u16v(t.qAppP50), p95: u16v(t.qAppP95), drops: [] },
    {
      id: "tx", label: "TX", unit: "frames", p50: u16v(t.qTxP50), p95: u16v(t.qTxP95),
      drops: [{ key: "drop_tx_overflow", label: "overflow", value: u32v(t.dropTxOverflow) }],
    },
    {
      id: "crl", label: "CRL", unit: "tasks", p50: u16v(t.qCrlP50), p95: u16v(t.qCrlP95),
      drops: [{ key: "drop_crl_backlog", label: "backlog", value: u32v(t.dropCrlBacklog) }],
    },
  ];
}

/** The complete §3.5.2 record as HUD groups, in the order of the 09-ui §5 sketch. */
export function hudGroups(t: NodeTelemetry): HudGroup[] {
  const nextTopup = isU64Sentinel(t.nextTopupNs) ? NA : durationNs(t.nextTopupNs);
  return [
    {
      id: "messages",
      title: "Messages and verification",
      fields: [
        { key: "msgs_in_per_s", label: "rx", value: `${num(t.msgsInPerS, 0)} msg/s`, raw: f32v(t.msgsInPerS), unit: "1/s", visibility: "NODE", help: "Messages delivered to the stack over the sampling window." },
        { key: "msgs_out_per_s", label: "tx", value: `${num(t.msgsOutPerS, 1)} msg/s`, raw: f32v(t.msgsOutPerS), unit: "1/s", visibility: "NODE" },
        { key: "verifications_per_s", label: "verify", value: `${num(t.verificationsPerS, 0)} /s`, raw: f32v(t.verificationsPerS), unit: "1/s", visibility: "NODE", help: "Signature verifications completed per second." },
        { key: "q_verify_p95", label: "verify queue p95", value: countU16(t.qVerifyP95), raw: u16v(t.qVerifyP95), unit: "messages", visibility: "NODE" },
        { key: "verify_wait_p95_ms", label: "verify wait p95", value: `${num(t.verifyWaitP95Ms, 1)} ms`, raw: f32v(t.verifyWaitP95Ms), unit: "ms", visibility: "NODE", help: "Enqueue → verification start, 95th percentile." },
        { key: "verify_wait_p50_ms", label: "verify wait p50", value: `${num(t.verifyWaitP50Ms, 1)} ms`, raw: f32v(t.verifyWaitP50Ms), unit: "ms", visibility: "NODE" },
        { key: "verify_policy", label: "policy", value: verifyPolicy(t.verifyPolicy), raw: t.verifyPolicy, unit: "enum", visibility: "NODE" },
        { key: "unverified_ratio_pm", label: "unverified delivered", value: permillePct(t.unverifiedRatioPm), raw: u16v(t.unverifiedRatioPm), unit: "per-mille", visibility: "NODE", help: "Share of messages handed to applications without verification." },
        { key: "airtime_ms_per_s", label: "air time", value: `${num(t.airtimeMsPerS, 1)} ms/s`, raw: f32v(t.airtimeMsPerS), unit: "ms/s", visibility: "NODE" },
        { key: "full_cert_msgs", label: "full-cert msgs", value: countU32(t.fullCertMsgs), raw: u32v(t.fullCertMsgs), unit: "count", visibility: "NODE" },
        { key: "p2pcd_requests", label: "P2PCD requests", value: countU32(t.p2pcdRequests), raw: u32v(t.p2pcdRequests), unit: "count", visibility: "NODE" },
      ],
    },
    {
      id: "compute",
      title: "Compute and storage",
      fields: [
        { key: "cpu_util_pm", label: "CPU", value: permillePct(t.cpuUtilPm), raw: u16v(t.cpuUtilPm), unit: "per-mille", visibility: "NODE", help: "Per-mille busy, averaged over cores." },
        { key: "hsm_util_pm", label: "HSM", value: permillePct(t.hsmUtilPm), raw: u16v(t.hsmUtilPm), unit: "per-mille", visibility: "NODE" },
        { key: "ram_used_kib", label: "RAM", value: `${kib(t.ramUsedKib)} / ${kib(t.ramTotalKib)}`, raw: ratioPct(t.ramUsedKib, t.ramTotalKib), unit: "KiB", visibility: "NODE", help: "Stores + queues + the hardware profile's baseline." },
        { key: "storage_used_b", label: "Flash", value: `${bytes(t.storageUsedB)} / ${bytes(t.storageTotalB)}`, raw: ratioPct(Number(t.storageUsedB), Number(t.storageTotalB)), unit: "bytes", visibility: "NODE" },
        { key: "node_state", label: "Node state", value: nodeState(t.nodeState), raw: t.nodeState, unit: "enum", visibility: t.nodeState === 6 ? "GT" : "NODE", help: "What the radio is doing. \"Compromised\" is something only the run itself knows, so it is withheld from anything being evaluated." },
      ],
    },
    {
      id: "certs",
      title: "Certificates, peers and CRL",
      fields: [
        { key: "cert_active", label: "Active pseudonyms", value: countU16(t.certActive), raw: u16v(t.certActive), unit: "count", visibility: "NODE" },
        { key: "cert_stored", label: "Certificates stored", value: countU32(t.certStored), raw: u32v(t.certStored), unit: "count", visibility: "NODE" },
        { key: "next_topup_ns", label: "Next top-up", value: nextTopup, raw: isU64Sentinel(t.nextTopupNs) ? null : Number(t.nextTopupNs), unit: "sim ns", visibility: "NODE", help: "Sim time of the next certificate top-up; u64::MAX means none scheduled." },
        { key: "peer_cache_entries", label: "Peer cache", value: countU32(t.peerCacheEntries), raw: u32v(t.peerCacheEntries), unit: "count", visibility: "NODE" },
        { key: "crl_entries", label: "CRL entries", value: countU32(t.crlEntries), raw: u32v(t.crlEntries), unit: "count", visibility: "NODE" },
        { key: "crl_bytes", label: "CRL bytes", value: bytes(t.crlBytes), raw: isU64Sentinel(t.crlBytes) ? null : Number(t.crlBytes), unit: "bytes", visibility: "NODE" },
        { key: "crl_expansion_pm", label: "CRL expansion", value: permillePct(t.crlExpansionPm), raw: u16v(t.crlExpansionPm), unit: "per-mille", visibility: "NODE", help: "Share of the current i-period linkage-value expansion completed." },
      ],
    },
    {
      id: "neighbours",
      title: "Neighbours, channel and DCC",
      fields: [
        { key: "nbr_total", label: "Neighbours", value: countU16(t.nbrTotal), raw: u16v(t.nbrTotal), unit: "count", visibility: "NODE" },
        { key: "nbr_verified", label: "verified", value: countU16(t.nbrVerified), raw: u16v(t.nbrVerified), unit: "count", visibility: "NODE" },
        { key: "nbr_unverified", label: "unverified", value: countU16(t.nbrUnverified), raw: u16v(t.nbrUnverified), unit: "count", visibility: "NODE" },
        { key: "nbr_revoked", label: "revoked", value: countU16(t.nbrRevoked), raw: u16v(t.nbrRevoked), unit: "count", visibility: "NODE" },
        { key: "cbr_pm", label: "CBR", value: permilleRatio(t.cbrPm), raw: u16v(t.cbrPm), unit: "per-mille", visibility: "NODE", help: "Channel busy ratio over the sampling window." },
        { key: "dcc_state", label: "DCC", value: dccState(t.dccState), raw: u16v(t.dccState), unit: "enum", visibility: "NODE" },
        { key: "tx_power_cdbm", label: "TX power", value: cdbm(t.txPowerCdbm), raw: t.txPowerCdbm / 100, unit: "centi-dBm", visibility: "NODE" },
      ],
    },
    {
      id: "gnss",
      title: "GNSS and clock",
      fields: [
        { key: "gnss_fix", label: "Fix", value: gnssFix(t.gnssFix), raw: t.gnssFix, unit: "enum", visibility: "NODE" },
        { key: "gnss_sigma_m", label: "σ horizontal", value: `${num(t.gnssSigmaM, 2)} m`, raw: f32v(t.gnssSigmaM), unit: "m", visibility: "NODE", help: "1σ horizontal error from the GNSS model's own noise parameters." },
        { key: "gnss_hdop", label: "HDOP", value: num(t.gnssHdop, 2), raw: f32v(t.gnssHdop), unit: "1", visibility: "NODE" },
        { key: "clock_drift_ppm", label: "Clock drift", value: `${num(t.clockDriftPpm, 2)} ppm`, raw: f32v(t.clockDriftPpm), unit: "ppm", visibility: "NODE" },
        { key: "clock_offset_ns", label: "Clock offset", value: signedNs(t.clockOffsetNs), raw: isU64Sentinel(t.clockOffsetNs) ? null : Number(t.clockOffsetNs), unit: "ns", visibility: "GT", help: "How far this radio's clock is from the true time. Only the run knows this, so it is withheld from anything being evaluated." },
        { key: "pos_error_m", label: "Belief vs truth", value: `${num(t.posErrorM, 2)} m`, raw: f32v(t.posErrorM), unit: "m", visibility: "GT", help: "How far this radio thinks it is from where it actually is, horizontally. Only the run knows this." },
      ],
    },
    {
      id: "reports",
      title: "Evidence and reports",
      fields: [
        { key: "outbox_msgs", label: "Report outbox", value: countU32(t.outboxMsgs), raw: u32v(t.outboxMsgs), unit: "count", visibility: "NODE", help: "Misbehaviour reports pending store-and-forward." },
        { key: "outbox_bytes", label: "Outbox bytes", value: bytes(t.outboxBytes), raw: isU64Sentinel(t.outboxBytes) ? null : Number(t.outboxBytes), unit: "bytes", visibility: "NODE" },
      ],
    },
    {
      id: "drops",
      title: "Drops by cause",
      fields: [
        { key: "drop_rx_overflow", label: "RX overflow", value: countU32(t.dropRxOverflow), raw: u32v(t.dropRxOverflow), unit: "count", visibility: "NODE" },
        { key: "drop_verify_overflow", label: "Verify overflow", value: countU32(t.dropVerifyOverflow), raw: u32v(t.dropVerifyOverflow), unit: "count", visibility: "NODE" },
        { key: "drop_verify_policy_skip", label: "Verify policy skip", value: countU32(t.dropVerifyPolicySkip), raw: u32v(t.dropVerifyPolicySkip), unit: "count", visibility: "NODE" },
        { key: "drop_tx_overflow", label: "TX overflow", value: countU32(t.dropTxOverflow), raw: u32v(t.dropTxOverflow), unit: "count", visibility: "NODE" },
        { key: "drop_reassembly_timeout", label: "Reassembly timeout", value: countU32(t.dropReassemblyTimeout), raw: u32v(t.dropReassemblyTimeout), unit: "count", visibility: "NODE" },
        { key: "drop_crl_backlog", label: "CRL backlog", value: countU32(t.dropCrlBacklog), raw: u32v(t.dropCrlBacklog), unit: "count", visibility: "NODE" },
      ],
    },
  ];
}

/** Total messages dropped, across all six reported causes, for the one-line summary. */
export function totalDrops(t: NodeTelemetry): number {
  const parts = [t.dropRxOverflow, t.dropVerifyOverflow, t.dropVerifyPolicySkip, t.dropTxOverflow, t.dropReassemblyTimeout, t.dropCrlBacklog];
  let sum = 0;
  for (const p of parts) if (!isU32Sentinel(p) && Number.isFinite(p)) sum += p;
  return sum;
}

/** The five series the 09-ui §5 sparkline row names, in its order. */
export const SPARKLINE_SERIES = [
  { key: "msgs_in_per_s", label: "rx/s", unit: "1/s", pick: (t: NodeTelemetry) => f32v(t.msgsInPerS) },
  { key: "q_verify_p95", label: "verify q", unit: "messages", pick: (t: NodeTelemetry) => u16v(t.qVerifyP95) },
  { key: "cpu_util_pm", label: "CPU %", unit: "%", pick: (t: NodeTelemetry) => (isU16Sentinel(t.cpuUtilPm) ? null : t.cpuUtilPm / 10) },
  { key: "cbr_pm", label: "CBR", unit: "ratio", pick: (t: NodeTelemetry) => (isU16Sentinel(t.cbrPm) ? null : t.cbrPm / 1000) },
  { key: "nbr_total", label: "neighbours", unit: "count", pick: (t: NodeTelemetry) => u16v(t.nbrTotal) },
] as const;

/** A compact `used/total` line for the identity strip. */
export function storageLine(t: NodeTelemetry): string {
  const ram = ratioPct(t.ramUsedKib, t.ramTotalKib);
  const flash = ratioPct(Number(t.storageUsedB), Number(t.storageTotalB));
  const ramPct = ram === null ? NA : `${ram.toFixed(0)} %`;
  const flashPct = flash === null ? NA : `${flash.toFixed(0)} %`;
  return `RAM ${ramPct} · flash ${flashPct} · ${int(t.certStored)} certs`;
}
