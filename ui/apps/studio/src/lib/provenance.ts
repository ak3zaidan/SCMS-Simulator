/**
 * The explainability surface: how *any* number on screen resolves to what produced it.
 *
 * The project's first hard rule is that a value can always say where it came from, and the Studio
 * already kept it in one place — every HUD field is a button that opens the inspector's "why" tab
 * (09-ui §5, §10). This module is what extends that to the rest of the app: the plots strip, the
 * overlay menu, the frame-rate readouts, the world summary, the frame counters and the scrub-bar
 * markers.
 *
 * # Two kinds of provenance, kept apart
 *
 * A value on screen came from one of two places, and conflating them would be the dishonest
 * shortcut:
 *
 *  * **The engine.** It carries a `prov_id` on the wire (§3.7 metric samples, several §3.6 event
 *    payloads) and resolves through the `Provenance` frames of §3.8 into a model id, a version, a
 *    parameter set and a model-card URL; or it carries none and `explain` (§6.9) resolves it
 *    server-side. `WhyTab` already handles both.
 *  * **The browser.** Frames per second, draw calls, the interpolation the viewer does between two
 *    deltas, the shape of a pulse ring — these are computed *here*, by `@vwp/viewer` or by this
 *    app, from engine inputs. They have no model card because there is no model: naming one would
 *    invent a citation. {@link ClientProvenance} states the producer, the computation, the inputs
 *    it was computed from and the rule it follows, and `WhyTab` renders it as what it is.
 *
 * Every entry below names a specification section or a source file. Nothing here says "computed
 * from telemetry" without saying which field.
 */

import type { OverlayName } from "@vwp/protocol";

import type { WhySubject } from "../state/store.js";

/**
 * Provenance for a value the browser produced.
 *
 * The analogue of a model card for a client-side computation — and deliberately not shaped like
 * one, so it cannot be mistaken for a model the engine would stand behind.
 */
export interface ClientProvenance {
  /** What computed it, as a package and a class: `@vwp/viewer · FrameStats`. */
  readonly producer: string;
  /** One sentence: the computation, in terms of its inputs. */
  readonly computation: string;
  /** Where the inputs came from — a wire field, an RPC result, or the renderer itself. */
  readonly inputs: string;
  /** The specification section or design document the computation follows, when there is one. */
  readonly reference?: string;
  /** What is rounded or quantised on the way to the screen, if anything. */
  readonly quantisation?: string;
  /** What the number does **not** mean; the caveat a reader would otherwise have to guess. */
  readonly caveat?: string;
}

/**
 * The client-measured values, by the `id` their {@link WhySubject} carries.
 *
 * Keys are the field names the readouts use, so a new readout that reuses a key inherits its
 * provenance and a new key with no entry is visible as a gap rather than silently unexplained
 * ({@link clientProvenance} returns `null`, and `WhyTab` says so).
 */
export const CLIENT_PROVENANCE: Readonly<Record<string, ClientProvenance>> = {
  fps: {
    producer: "@vwp/viewer · FrameStats",
    computation: "Presented frames divided by the wall-clock span of the ring's samples.",
    inputs: "The renderer's own frame callbacks; no engine value is involved.",
    reference: "09-ui §4 (60 fps presentation budget)",
    caveat: "A presentation rate, not a simulation rate: a paused run still renders at full speed.",
  },
  fpsAverage: {
    producer: "@vwp/viewer · FrameStats",
    computation: "Mean of the frame rate over the retained window (240 frames).",
    inputs: "The renderer's frame callbacks.",
    reference: "09-ui §4",
  },
  frameMs: {
    producer: "@vwp/viewer · FrameStats",
    computation: "Wall-clock milliseconds between the last two presented frames.",
    inputs: "The renderer's frame callbacks.",
    reference: "09-ui §4",
  },
  p95Ms: {
    producer: "@vwp/viewer · FrameStats",
    computation: "95th percentile of the retained frame times, by rank on the sorted window.",
    inputs: "The renderer's frame callbacks.",
    reference: "09-ui §4 (the budget is stated as a p95, not a mean)",
  },
  cpuMs: {
    producer: "@vwp/viewer · FrameStats",
    computation: "Milliseconds spent inside the viewer's own update and draw, excluding GPU time.",
    inputs: "Timestamps taken around the viewer's frame body.",
    reference: "09-ui §4",
    caveat: "Not GPU time; a frame can be slow with this number small.",
  },
  drawCalls: {
    producer: "@vwp/viewer · FrameStats",
    computation: "The renderer's own draw-call counter for the last frame.",
    inputs: "`WebGLRenderer.info.render` (Three.js).",
  },
  triangles: {
    producer: "@vwp/viewer · FrameStats",
    computation: "The renderer's triangle count for the last frame.",
    inputs: "`WebGLRenderer.info.render` (Three.js).",
  },
  actorInstances: {
    producer: "@vwp/viewer · ActorRenderer",
    computation: "Instances written to the instanced mesh this frame, after frustum and LOD culling.",
    inputs: "The pose buffer of §3.3/§3.4 and the camera.",
    reference: "09-ui §3 (instanced actors, LOD ladder)",
    caveat: "Fewer than the live actors is culling, not actors leaving the run.",
  },
  actorCulled: {
    producer: "@vwp/viewer · ActorRenderer",
    computation: "Occupied pose slots the frustum or the LOD ladder rejected.",
    inputs: "The pose buffer and the camera.",
  },
  actorLive: {
    producer: "@vwp/viewer · ActorRenderer",
    computation: "Occupied slots in the pose buffer — actors the stream says exist.",
    inputs: "`PoseBuffer.occupied`, seeded by §3.3 keyframes and advanced by §3.4 deltas.",
  },
  buildingsVisible: {
    producer: "@vwp/viewer · WorldRenderer",
    computation: "Building volumes inside the frustum after the per-tile cull.",
    inputs: "The `vwp-world/1` building section (§4.4).",
  },
  interpolationAlpha: {
    producer: "@vwp/viewer · PoseInterpolator",
    computation:
      "Fraction between the two most recent pose snapshots the on-screen position is drawn at.",
    inputs: "The arrival clock of §3.3/§3.4 frames and the nominal `mobility_step_ns` of §3.1.1.",
    reference: "09-ui §3 (interpolate, never extrapolate past one step)",
    caveat: "Positions on screen between two deltas are interpolated, not simulated.",
  },
  frames_received: {
    producer: "@vwp/studio · StudioEngine",
    computation: "A count of decoded frames, by §2.4 message type, since the connection opened.",
    inputs: "The VWP frame headers themselves.",
    reference: "vwp-v1 §2.4",
    caveat: "Counts what this client decoded, which after a §1.5 drop is fewer than what was produced.",
  },
  replay_position: {
    producer: "v2xw-wasm · ReplaySession (compiled to WebAssembly)",
    computation:
      "The recorded keyframe at or before the seek target, with every recorded delta up to the target applied.",
    inputs: "The recording's chunk index and its `Keyframe`/`Delta` frames, byte for byte.",
    reference: "vwp-v1 §7.3, 09-ui §7 (one reader, two targets)",
    caveat: "A recording carries no world payload (§7.1); geometry must come from elsewhere.",
  },
  compare_time: {
    producer: "@vwp/studio · CompareController",
    computation:
      "Side B's simulated time after the last seek, which is side A's time plus the alignment offset, clamped to B's own span.",
    inputs: "`run.seek`'s reply (§6.6) for a second engine, or the WebAssembly reader's resolved position (§7.3).",
    reference: "09-ui §6 (synchronised time)",
    caveat: "A non-zero offset is an alignment the researcher asserted, not one the two runs agreed on.",
  },
  compare_delta: {
    producer: "@vwp/studio · CompareController",
    computation:
      "Side B's value minus side A's, both read at the same simulated time from each side's own metric history.",
    inputs: "`MetricSample` frames (§3.7) from each side's connection.",
    reference: "09-ui §6 (difference overlays for metrics)",
    caveat:
      "A difference is only meaningful when the two runs share a metric definition; the definition is not compared, only the name.",
  },
};

/** The client provenance for `id`, or `null` when that readout has not declared one. */
export function clientProvenance(id: string): ClientProvenance | null {
  return CLIENT_PROVENANCE[id] ?? null;
}

/**
 * What feeds each overlay, and what it draws.
 *
 * The `channels` column is the same mapping the engine reports in `overlay.set {list:true}`'s
 * `needs_channels` (`overlay_channels` in `crates/v2xw-server/src/rpc.rs`), so an overlay that is
 * on while its channel is unsubscribed is explainable rather than mysteriously empty. An overlay
 * with no channels is drawn from the world payload or the pose buffer, which the summary says.
 */
export const OVERLAY_SOURCES: Readonly<Record<OverlayName, { readonly channels: readonly string[]; readonly summary: string }>> = {
  tx_pulses: { channels: ["node.tx"], summary: "One expanding ring per sampled transmission, at the transmitting node's position." },
  links: { channels: ["phy.rx"], summary: "A segment per delivered reception, between the transmitting and receiving nodes." },
  cbr_heatmap: { channels: ["mac.cbr"], summary: "Channel-busy ratio per node, splatted onto the ground plane." },
  coverage: { channels: [], summary: "Modelled reception probability around each roadside site, from the site table of §4.5." },
  attackers_gt: { channels: ["gt.kinematics"], summary: "Ground-truth attacker identity, straight from the run's own knowledge." },
  revoked: { channels: ["proto.revocation"], summary: "A marker on each node a revocation event has named." },
  reported: { channels: ["proto.revocation"], summary: "A marker on each node a misbehaviour report has named — a belief, not ground truth." },
  detections: { channels: ["det.observation"], summary: "A marker per detector observation, at the observed node." },
  backend_flows: { channels: [], summary: "Backend message flows between SCMS entities, from the entity table." },
  focus_region: { channels: [], summary: "The fidelity-ladder region boundary the scenario declared." },
  lane_markings: { channels: [], summary: "Lane centre lines and edges from the `vwp-world/1` lane section (§4.3)." },
  buildings: { channels: [], summary: "Extruded building volumes from the `vwp-world/1` building section (§4.4)." },
  labels: { channels: [], summary: "Text labels for actors and sites, positioned from the pose buffer." },
  trajectories_gt: { channels: ["gt.kinematics"], summary: "Ground-truth path history per actor." },
  belief_vs_truth_gt: { channels: ["gt.kinematics"], summary: "A segment from each node's believed position to its true one." },
  signal_state: { channels: [], summary: "Signal phase and time-to-change from the §3.3.3 signal block of each keyframe." },
  rsu_range: { channels: [], summary: "A nominal range ring per roadside site, from the site table of §4.5." },
  density: { channels: [], summary: "Actor density per ground cell, counted from the pose buffer." },
};

/** Every overlay whose data is ground truth, by the `_gt` suffix §6.7 defines. */
export function isGroundTruthOverlayName(name: string): boolean {
  return name.endsWith("_gt");
}

/**
 * The provenance of one overlay as a client-side record.
 *
 * An overlay is drawn here, from engine inputs, so this is {@link ClientProvenance} — but the
 * *inputs* are named precisely, which is what makes an empty overlay diagnosable: no `node.tx`
 * subscription means no pulses, and the panel can say so instead of looking broken.
 */
export function overlayProvenance(name: string): ClientProvenance {
  const known = (OVERLAY_SOURCES as Readonly<Record<string, { readonly channels: readonly string[]; readonly summary: string }>>)[name];
  const channels = known?.channels ?? [];
  const inputs =
    channels.length > 0
      ? `\`Event\` frames on ${channels.join(", ")} (§3.6); nothing is drawn while those channels are unsubscribed (§6.12).`
      : "The decoded world payload (§4) and the pose buffer (§3.3/§3.4); no event subscription is needed.";
  return {
    producer: "@vwp/viewer · OverlayManager",
    computation: known?.summary ?? `The \`${name}\` overlay; this build has no description for it.`,
    inputs,
    reference: "vwp-v1 §6.7, 09-ui §6",
    ...(isGroundTruthOverlayName(name)
      ? { caveat: "Ground truth: a node cannot see this, so it must be off for a blind evaluation (09-ui §6)." }
      : {}),
  };
}

/** What each `vwp-world/1` summary figure counts, by the key the world summary uses (§4). */
export const WORLD_FIELDS: Readonly<Record<string, { readonly label: string; readonly section: string }>> = {
  lanes: { label: "lanes", section: "§4.3 lane section" },
  buildings: { label: "buildings", section: "§4.4 building section" },
  junctions: { label: "junctions", section: "§4.5 junction section" },
  signals: { label: "signal heads", section: "§4.5 signal section" },
  sites: { label: "roadside sites", section: "§4.5 site section" },
  crossings: { label: "crossings", section: "§4.5 crossing section" },
  landuse: { label: "land-use polygons", section: "§4.5 land-use section" },
  bytes: { label: "payload size", section: "§4.1 file header" },
  buildMs: { label: "scene build time", section: "@vwp/viewer WorldRenderer" },
  drawables: { label: "drawables", section: "@vwp/viewer WorldRenderer" },
};

// ---------------------------------------------------------------------------------------------
// Subject builders — one per surface, so the `ValueRef` a panel sends is built in one place
// ---------------------------------------------------------------------------------------------

/** A metric on the plots strip (§3.7 sample, §6.9 `explain` subject kind `metric`). */
export function metricSubject(
  name: string,
  value: number | null,
  unit?: string,
  provId?: number,
): WhySubject {
  return {
    kind: "metric",
    id: name,
    label: name,
    ...(value === null ? {} : { value: String(value) }),
    ...(unit === undefined || unit === "" ? {} : { unit }),
    ...(provId === undefined || provId === 0 ? {} : { provId }),
  };
}

/** A client-measured readout: the frame-rate strip, the frame counters, the replay position. */
export function clientSubject(id: string, label: string, value?: string, unit?: string): WhySubject {
  const client = clientProvenance(id);
  return {
    kind: "entity",
    id,
    label,
    ...(value === undefined ? {} : { value }),
    ...(unit === undefined ? {} : { unit }),
    ...(client === null ? {} : { client }),
  };
}

/** One overlay in the overlay menu (§6.7, `explain` subject kind `overlay`). */
export function overlaySubject(name: string, enabled: boolean): WhySubject {
  return {
    kind: "overlay",
    id: name,
    label: name,
    value: enabled ? "on" : "off",
    client: overlayProvenance(name),
  };
}

/** One figure of the world summary (§4, `explain` subject kind `world`). */
export function worldSubject(field: string, value: string): WhySubject {
  const known = WORLD_FIELDS[field];
  return {
    kind: "world",
    id: field,
    label: known?.label ?? field,
    value,
    client: {
      producer: "@vwp/protocol · decodeWorld",
      computation: `Counted from the ${known?.section ?? "world payload"} of the decoded \`vwp-world/1\` payload.`,
      inputs: "The payload fetched from `/world/{hash}.vwb`, verified against `Hello.world_hash` (§10.5 W3).",
      reference: "vwp-v1 §4",
    },
  };
}

/** One event channel (§3.1.5 channel table, `explain` subject kind `channel`). */
export function channelSubject(name: string, note: string): WhySubject {
  return {
    kind: "channel",
    id: name,
    label: name,
    value: note,
    client: {
      producer: "@vwp/studio · StudioEngine",
      computation: `Frames decoded on the \`${name}\` channel since the connection opened.`,
      inputs: "The §3.6.1 event index's `channel_id` column, resolved through the §3.1.5 channel table.",
      reference: "vwp-v1 §3.6, §6.12",
    },
  };
}

/** One scrub-bar marker (§3.6 event, `explain` subject kind `event`). */
export function eventSubject(channel: string, label: string, nodeId: number, provId?: number): WhySubject {
  return {
    kind: "event",
    id: channel,
    label,
    node: nodeId,
    ...(provId === undefined || provId === 0 ? {} : { provId }),
  };
}

/** One side-by-side metric difference (09-ui §6 "difference overlays for metrics"). */
export function differenceSubject(name: string, delta: number | null, unit?: string): WhySubject {
  return {
    kind: "metric",
    id: name,
    label: `${name} · B − A`,
    ...(delta === null ? {} : { value: delta.toPrecision(6) }),
    ...(unit === undefined || unit === "" ? {} : { unit }),
    client: CLIENT_PROVENANCE.compare_delta,
  };
}
