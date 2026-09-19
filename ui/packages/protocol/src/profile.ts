/**
 * The `NODE-only` profile checks — docs/protocol/vwp-v1.md §5.
 *
 * §5.2 is an exhaustive list of what a `node`-profile server MUST NOT emit, and §10.6 V1 asks for a
 * `node_profile_leakage` test that "decodes the whole stream and asserts every GT field is at its
 * sentinel and every GT channel is absent". Before this file the profile existed only as doc
 * comments: the enums and flags were all present and correctly numbered, but nothing walked a
 * decoded frame and checked them, so the conformance kit had nowhere to get the GT set from.
 *
 * Both functions here are pure over already-decoded messages, so the Rust conformance suite and
 * this TypeScript client can share one definition of the GT set: a decoded frame in, a list of
 * human-readable violations out (empty means clean). They never throw on a leak — a leak is a
 * server conformance failure to report, not a malformed frame to close the socket on.
 */

import { FrameFlags, SENTINEL_U16, SENTINEL_U32, SENTINEL_U8, isCanonicalMsgType } from "./frame.js";
import {
  ActorState,
  ChannelId,
  HelloFlags,
  MovedFlags,
  NodeFlags,
  type HelloMessage,
  type VwpMessage,
  eventChannelName,
} from "./messages.js";

/** §5.2 — channels a `node`-profile server withholds entirely. */
export const GT_CHANNEL_IDS: readonly number[] = [
  ChannelId.GT_KINEMATICS,
  ChannelId.GT_ATTACK_ACTION,
  ChannelId.GT_SPAWN,
  ChannelId.GT_DESPAWN,
];

/** §3.6.10 / §5.3 — under the `node` profile only `issued` (5) and later revocation stages appear. */
export const REVOCATION_PUBLIC_STAGE_FROM = 5;

/**
 * §5.2 / §10.6 V1 — every §5.2 GT field that is not at its blanked value, plus every GT channel
 * that is present, in one decoded frame. An empty array means this frame is clean.
 *
 * Call it on every frame of a `profile=node` stream; the union over the stream is the V1 verdict.
 */
export function checkNodeProfileLeakage(msg: VwpMessage): string[] {
  const out: string[] = [];
  switch (msg.kind) {
    case "hello": {
      for (let i = 0; i < msg.nodes.count; i++) {
        if ((msg.nodes.flags[i] & NodeFlags.IS_ATTACKER) !== 0) {
          out.push(`Hello.nodes[${i}].flags has IS_ATTACKER set (§5.2: MUST be 0)`);
        }
      }
      for (let i = 0; i < msg.channels.count; i++) {
        const id = msg.channels.channelId[i];
        if (GT_CHANNEL_IDS.includes(id)) {
          out.push(`Hello.channels lists the GT channel ${eventChannelName(id)} (${id}); §5.2 requires it to be absent, not merely disabled`);
        }
      }
      break;
    }
    case "keyframe": {
      const a = msg.actors;
      for (let s = 0; s < a.count; s++) {
        if (a.actorId[s] === SENTINEL_U32) continue;
        if (a.laneId[s] !== SENTINEL_U32) out.push(`Keyframe.actors[${s}].lane_id is ${a.laneId[s]} (§5.2: 0xFFFFFFFF)`);
        if (a.accelCq[s] !== 0) out.push(`Keyframe.actors[${s}].accel_cq is ${a.accelCq[s]} (§5.2: 0)`);
        if ((a.state[s] & ActorState.ATTACKER) !== 0) out.push(`Keyframe.actors[${s}].state has ST_ATTACKER set (§5.2: 0)`);
        if ((a.state[s] & ActorState.EQUIPPED) === 0) {
          out.push(`Keyframe.actors[${s}] is occupied without ST_EQUIPPED (§5.2: such a slot is left empty)`);
        }
      }
      break;
    }
    case "delta": {
      const m = msg.moved;
      for (let i = 0; i < m.count; i++) {
        if (m.accelCq[i] !== 0) out.push(`Delta.moved[${i}].accel_cq is ${m.accelCq[i]} (§5.2: 0)`);
        if ((m.state[i] & ActorState.ATTACKER) !== 0) out.push(`Delta.moved[${i}].state has ST_ATTACKER set (§5.2: 0)`);
        if ((m.mflags[i] & MovedFlags.LANE_CHANGED) !== 0) {
          out.push(`Delta.moved[${i}].mflags has MFLAG_LANE_CHANGED set (§5.2: clear under the node profile)`);
        }
      }
      if (msg.lanes.length > 0) out.push(`Delta carries a ${msg.lanes.length}-entry lane block (§5.2: absent, lane_count = 0)`);
      const sp = msg.spawns;
      for (let i = 0; i < sp.count; i++) {
        if (sp.laneId[i] !== SENTINEL_U32) out.push(`Delta.spawns[${i}].lane_id is ${sp.laneId[i]} (§5.2: 0xFFFFFFFF)`);
        if (sp.cause[i] !== SENTINEL_U16) out.push(`Delta.spawns[${i}].cause is ${sp.cause[i]} (§5.2: 0xFFFF)`);
        if ((sp.state[i] & ActorState.ATTACKER) !== 0) out.push(`Delta.spawns[${i}].state has ST_ATTACKER set (§5.2: 0)`);
        if ((sp.state[i] & ActorState.EQUIPPED) === 0) {
          out.push(`Delta.spawns[${i}] has no ST_EQUIPPED (§5.2: an unequipped actor's slot is left empty)`);
        }
      }
      const dp = msg.despawns;
      for (let i = 0; i < dp.count; i++) {
        if (dp.cause[i] !== SENTINEL_U16) out.push(`Delta.despawns[${i}].cause is ${dp.cause[i]} (§5.2: 0xFFFF)`);
      }
      break;
    }
    case "telemetry": {
      for (let i = 0; i < msg.nodeCount; i++) {
        const r = msg.record(i);
        if (r.clockOffsetNs !== 0n) out.push(`Telemetry[${i}].clock_offset_ns is ${r.clockOffsetNs} (§5.2: 0)`);
        if (!Number.isNaN(r.posErrorM)) out.push(`Telemetry[${i}].pos_error_m is ${r.posErrorM} (§5.2: NaN)`);
        if (r.nodeState === 6) out.push(`Telemetry[${i}].node_state is 6 (compromised); §5.2 requires it reported as 2 (active)`);
      }
      break;
    }
    case "event": {
      for (let i = 0; i < msg.count; i++) {
        const id = msg.index.channelId[i];
        if (GT_CHANNEL_IDS.includes(id)) {
          out.push(`Event[${i}] is on the GT channel ${eventChannelName(id)} (${id}); §5.2 withholds it`);
          continue;
        }
        const p = msg.payload(i);
        switch (p.channel) {
          case "phy.rx":
            if (p.txNode !== SENTINEL_U32) out.push(`Event[${i}] phy.rx.tx_node is ${p.txNode} (§5.2: 0xFFFFFFFF)`);
            if (!Number.isNaN(p.distanceM)) out.push(`Event[${i}] phy.rx.distance_m is ${p.distanceM} (§5.2: NaN)`);
            if (p.losClass !== SENTINEL_U8) out.push(`Event[${i}] phy.rx.los_class is ${p.losClass} (§5.2: 0xFF)`);
            break;
          case "node.neighbor":
            if (p.peerActorId !== SENTINEL_U32) out.push(`Event[${i}] node.neighbor.peer_actor_id is ${p.peerActorId} (§5.2: 0xFFFFFFFF)`);
            break;
          case "det.observation":
            if (p.subjectActorId !== SENTINEL_U32) out.push(`Event[${i}] det.observation.subject_actor_id is ${p.subjectActorId} (§5.2: 0xFFFFFFFF)`);
            break;
          case "app.warning":
            if (p.truth !== 0) out.push(`Event[${i}] app.warning.truth is ${p.truth} (§5.2: 0)`);
            if (p.subjectActorId !== SENTINEL_U32) out.push(`Event[${i}] app.warning.subject_actor_id is ${p.subjectActorId} (§5.2: 0xFFFFFFFF)`);
            break;
          case "ma.report":
          case "ma.case":
          case "ma.decision":
            if (p.subjectActorId !== SENTINEL_U32) out.push(`Event[${i}] ${p.channel}.subject_actor_id is ${p.subjectActorId} (§5.2: 0xFFFFFFFF)`);
            break;
          case "proto.revocation":
            if (p.stage < REVOCATION_PUBLIC_STAGE_FROM) {
              out.push(`Event[${i}] proto.revocation is at stage ${p.stage}; §5.2/§5.3 withhold stages 0–4`);
            }
            break;
          default:
            break;
        }
      }
      break;
    }
    case "metric": {
      for (let i = 0; i < msg.sampleCount; i++) {
        const s = msg.sample(i);
        if (s.visibility === 0) out.push(`MetricSample[${i}] (str_metric ${s.strMetric}) has visibility 0 (GT); §5.2 does not emit it`);
      }
      break;
    }
    default:
      break;
  }
  return out;
}

/**
 * §5.3 — `HELLO_NODE_ONLY`, `Keyframe.profile` and `FLAG_NODE_ONLY` must agree with each other.
 *
 * "The server sets `HELLO_NODE_ONLY` in `hello_flags`, `Keyframe.profile = 1`, and `FLAG_NODE_ONLY`
 * on every canonical frame." Nothing checked that the three say the same thing, so a stream that
 * blanked GT but forgot the flag — or set the flag without blanking — looked fine.
 */
export function checkProfileConsistency(hello: HelloMessage, msg: VwpMessage): string[] {
  const out: string[] = [];
  const nodeOnly = (hello.helloFlags & HelloFlags.NODE_ONLY) !== 0;
  if (msg.kind === "hello") return out;
  if (isCanonicalMsgType(msg.header.msgType)) {
    const frameNodeOnly = (msg.header.flags & FrameFlags.NODE_ONLY) !== 0;
    if (frameNodeOnly !== nodeOnly) {
      out.push(
        `frame seq ${msg.header.seq} (msg_type 0x${msg.header.msgType.toString(16).padStart(4, "0")}) has FLAG_NODE_ONLY ${
          frameNodeOnly ? "set" : "clear"
        } but Hello has HELLO_NODE_ONLY ${nodeOnly ? "set" : "clear"} (§5.3)`,
      );
    }
  }
  if (msg.kind === "keyframe") {
    const keyframeNodeOnly = msg.profile === 1;
    if (keyframeNodeOnly !== nodeOnly) {
      out.push(`Keyframe.profile is ${msg.profile} but Hello has HELLO_NODE_ONLY ${nodeOnly ? "set" : "clear"} (§5.3)`);
    }
  }
  return out;
}
