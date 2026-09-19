/** Test helpers: encode a body with `src/encode.ts`, then decode it back through the real path. */

import {
  type DeltaInit,
  type NodeTelemetryInit,
  type DeltaMessage,
  type KeyframeInit,
  type KeyframeMessage,
  decodeDelta,
  decodeKeyframe,
  deltaFrame,
  keyframeFrame,
  viewFrame,
} from "../../src/index.js";

/** Encode a `Keyframe` from plain values and hand back the decoded message. */
export function buildKeyframe(init: KeyframeInit, seq = 0n, flags = 0): KeyframeMessage {
  return decodeKeyframe(viewFrame(keyframeFrame(init, seq, flags)));
}

/** Encode a `Delta` from plain values and hand back the decoded message. */
export function buildDelta(init: DeltaInit, seq = 1n, flags = 0): DeltaMessage {
  return decodeDelta(viewFrame(deltaFrame(init, seq, flags)));
}

/** A §3.5.2 record with every field populated or at its documented sentinel (§10.4 C1). */
export const telemetryRow = (nodeId: number): NodeTelemetryInit => ({
  storageUsedB: 4_194_304n, storageTotalB: 67_108_864n, nextTopupNs: 86_400_000_000_000n,
  crlBytes: 131_072n, outboxBytes: 2_048n, clockOffsetNs: -1_250_000n,
  nodeId, ramUsedKib: 3_280, ramTotalKib: 65_536,
  dropRxOverflow: 12, dropVerifyPolicySkip: 340, dropVerifyOverflow: 7, dropTxOverflow: 1,
  dropReassemblyTimeout: 2, dropCrlBacklog: 0, certStored: 20, crlEntries: 812, outboxMsgs: 3,
  peerCacheEntries: 96, p2pcdRequests: 4, fullCertMsgs: 11,
  msgsInPerS: 412.5, msgsOutPerS: 10, verificationsPerS: 380.25, verifyWaitP50Ms: 1.5,
  verifyWaitP95Ms: 7.25, gnssHdop: 0.9, gnssSigmaM: 1.4, clockDriftPpm: 3.5, posErrorM: 0.62,
  airtimeMsPerS: 42.75, cpuUtilPm: 615, hsmUtilPm: 220,
  qRxP50: 3, qRxP95: 18, qVerifyP50: 5, qVerifyP95: 41, qAppP50: 1, qAppP95: 6,
  qTxP50: 0, qTxP95: 2, qCrlP50: 0, qCrlP95: 1,
  dccState: 1, cbrPm: 372, txPowerCdbm: 2_000,
  nbrTotal: 64, nbrVerified: 51, nbrUnverified: 12, nbrRevoked: 1, certActive: 20,
  crlExpansionPm: 1_000, unverifiedRatioPm: 180,
  gnssFix: 2, nodeState: 2, verifyPolicy: 2,
});
