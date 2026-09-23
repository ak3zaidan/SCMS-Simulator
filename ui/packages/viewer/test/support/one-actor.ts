/**
 * A pose buffer holding a handful of actors at positions the test chooses.
 *
 * {@link SyntheticStream} scatters its crowd across the whole grid, which is the right fixture for
 * the budget and culling tests but the wrong one for framing: the interesting case is the shape of
 * `scenarios/phase1-manhattan.yaml` — one equipped vehicle, somewhere in a three-kilometre world,
 * nowhere near the centre of its bounding box. That is the case where an aerial view framed on the
 * world's centre shows no traffic at all, and it needs a placed actor rather than a random one.
 *
 * The bytes still go through the real encoder: `keyframeFrame` → `viewFrame` → `decodeKeyframe` →
 * `PoseBuffer`, so the quantisation the wire applies (§3.3) applies here too.
 */

import {
  ActorState,
  PoseBuffer,
  decodeKeyframe,
  keyframeFrame,
  quantiseAccelCq,
  quantiseHeadingBrad,
  quantiseHeightCm,
  quantisePositionMm,
  quantiseSpeedCq,
  viewFrame,
  type KeyframeInit,
} from "@vwp/protocol";

/** One actor to place, in ENU metres. */
export interface PlacedActor {
  readonly actorId: number;
  readonly x: number;
  readonly y: number;
  readonly z?: number;
  /** Radians, 0 = +x. */
  readonly headingRad?: number;
  readonly speedMps?: number;
  readonly classIdx?: number;
  readonly state?: number;
}

/** A pose buffer carrying exactly the actors given, at exactly those positions. */
export function placedPoses(actors: readonly PlacedActor[], simTimeNs = 0n): PoseBuffer {
  const poses = new PoseBuffer(Math.max(64, actors.length));
  const init: KeyframeInit = {
    simTimeNs,
    originXM: 0,
    originYM: 0,
    originZM: 0,
    gopIndex: 0,
    profile: 0,
    actors: actors.map((a) => ({
      actorId: a.actorId,
      xMm: quantisePositionMm(a.x),
      yMm: quantisePositionMm(a.y),
      zCm: quantiseHeightCm(a.z ?? 0),
      laneId: 0xffffffff,
      headingBrad: quantiseHeadingBrad(a.headingRad ?? 0),
      speedCq: quantiseSpeedCq(a.speedMps ?? 8),
      accelCq: quantiseAccelCq(0),
      classIdx: a.classIdx ?? 0,
      state: a.state ?? ActorState.EQUIPPED,
      verifiedNeighbors: 4,
    })),
    signals: [],
  };
  poses.applyKeyframe(decodeKeyframe(viewFrame(keyframeFrame(init, 1n))));
  return poses;
}
