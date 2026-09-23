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
  /**
   * The rule the computation follows, in words.
   *
   * Plain language on purpose: this row is read by someone studying vehicle communication, and
   * "09-ui §4" tells them nothing. The document and section live in {@link specRef}, which the
   * interface shows only with developer details switched on.
   */
  readonly reference?: string;
  /** Where that rule is written down: a document and a section, for whoever is checking it. */
  readonly specRef?: string;
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
    reference: "The Studio's budget of 60 drawn frames a second.",
    specRef: "09-ui §4",
    caveat: "A presentation rate, not a simulation rate: a paused run still renders at full speed.",
  },
  fpsAverage: {
    producer: "@vwp/viewer · FrameStats",
    computation: "Mean of the frame rate over the retained window (240 frames).",
    inputs: "The renderer's frame callbacks.",
    reference: "The Studio's budget of 60 drawn frames a second.",
    specRef: "09-ui §4",
  },
  frameMs: {
    producer: "@vwp/viewer · FrameStats",
    computation: "Wall-clock milliseconds between the last two presented frames.",
    inputs: "The renderer's frame callbacks.",
    reference: "The Studio's budget of 60 drawn frames a second, which is 16.7 ms a frame.",
    specRef: "09-ui §4",
  },
  p95Ms: {
    producer: "@vwp/viewer · FrameStats",
    computation: "95th percentile of the retained frame times, by rank on the sorted window.",
    inputs: "The renderer's frame callbacks.",
    reference: "The drawing budget is stated as a 95th percentile, not an average: the worst frames are what a viewer notices.",
    specRef: "09-ui §4",
  },
  cpuMs: {
    producer: "@vwp/viewer · FrameStats",
    computation: "Milliseconds spent inside the viewer's own update and draw, excluding GPU time.",
    inputs: "Timestamps taken around the viewer's frame body.",
    reference: "The Studio's drawing budget, of which this is the part spent on the processor.",
    specRef: "09-ui §4",
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
    inputs: "The positions the stream sends — full snapshots, and the incremental updates between them — and the camera.",
    reference: "Vehicles are drawn from one instanced mesh, with simpler shapes further from the camera.",
    specRef: "09-ui §3",
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
    inputs: "The occupied slots of the position buffer: seeded by each full snapshot and advanced by the incremental updates between them.",
  },
  buildingsVisible: {
    producer: "@vwp/viewer · WorldRenderer",
    computation: "Building volumes inside the frustum after the per-tile cull.",
    inputs: "The building outlines in the world file.",
  },
  interpolationAlpha: {
    producer: "@vwp/viewer · PoseInterpolator",
    computation:
      "Fraction between the two most recent pose snapshots the on-screen position is drawn at.",
    inputs: "When each position update arrived, and the interval the run says one movement step covers.",
    reference: "Positions are interpolated between two updates and never extrapolated past one step.",
    specRef: "09-ui §3",
    caveat: "Positions on screen between two deltas are interpolated, not simulated.",
  },
  frames_received: {
    producer: "@vwp/studio · StudioEngine",
    computation: "A count of frames this page decoded, by kind, since the connection opened.",
    inputs: "The frame headers of the stream itself.",
    reference: "The stream's own frame kinds: full snapshot, incremental update, radio telemetry, event, measurement.",
    specRef: "vwp-v1 §2.4",
    caveat: "Counts what this page received. When the engine drops frames to keep up, it produced more than this.",
  },
  replay_position: {
    producer: "v2xw-wasm · ReplaySession (compiled to WebAssembly)",
    computation:
      "The recorded keyframe at or before the seek target, with every recorded delta up to the target applied.",
    inputs: "The recording's chunk index and its `Keyframe`/`Delta` frames, byte for byte.",
    reference: "The recording is read by the same code the engine uses, compiled to run in the browser, so the positions it resolves are the recorded ones.",
    specRef: "vwp-v1 §7.3, 09-ui §7",
    caveat: "A recording does not contain the streets. The geometry on screen has to come from a world file or from a connection.",
  },
  compare_time: {
    producer: "@vwp/studio · CompareController",
    computation:
      "Side B's simulated time after the last seek, which is side A's time plus the alignment offset, clamped to B's own span.",
    inputs: "The second engine's answer to a seek, or the position the in-browser reader resolved.",
    reference: "Two runs scrubbed on one simulated clock.",
    specRef: "09-ui §6",
    caveat: "A non-zero offset is an alignment the researcher asserted, not one the two runs agreed on.",
  },
  compare_delta: {
    producer: "@vwp/studio · CompareController",
    computation:
      "Side B's value minus side A's, both read at the same simulated time from each side's own metric history.",
    inputs: "The measurement samples each side's engine sent.",
    reference: "Two runs compared measurement by measurement, at the same simulated time.",
    specRef: "09-ui §6",
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
  coverage: { channels: [], summary: "Modelled reception probability around each roadside unit, from the units listed in the world file." },
  attackers_gt: { channels: ["gt.kinematics"], summary: "Ground-truth attacker identity, straight from the run's own knowledge." },
  revoked: { channels: ["proto.revocation"], summary: "A marker on each node a revocation event has named." },
  reported: { channels: ["proto.revocation"], summary: "A marker on each node a misbehaviour report has named — a belief, not ground truth." },
  detections: { channels: ["det.observation"], summary: "A marker per detector observation, at the observed node." },
  backend_flows: { channels: [], summary: "Backend message flows between SCMS entities, from the entity table." },
  focus_region: { channels: [], summary: "The fidelity-ladder region boundary the scenario declared." },
  lane_markings: { channels: [], summary: "Lane centre lines and edges, from the lane geometry in the world file." },
  buildings: { channels: [], summary: "Extruded building volumes, from the building outlines in the world file." },
  labels: { channels: [], summary: "Text labels for actors and sites, positioned from the pose buffer." },
  trajectories_gt: { channels: ["gt.kinematics"], summary: "Ground-truth path history per actor." },
  belief_vs_truth_gt: { channels: ["gt.kinematics"], summary: "A segment from each node's believed position to its true one." },
  signal_state: { channels: [], summary: "Signal phase and time to the next change, from the signal state in each full snapshot." },
  rsu_range: { channels: [], summary: "A nominal range ring per roadside unit, from the units listed in the world file." },
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
      ? `Event frames on ${channels.join(", ")}. Nothing is drawn while the page is not subscribed to those, which is why this overlay can be on and empty at the same time.`
      : "The world file and the positions the stream sends. No event subscription is needed, so this overlay is never empty for want of one.";
  return {
    producer: "@vwp/viewer · OverlayManager",
    computation: known?.summary ?? `The \`${name}\` overlay; this build has no description for it.`,
    inputs,
    reference: "The engine publishes what it can offer to draw; this build draws what it can. An overlay either side lacks is shown as unavailable rather than quietly missing.",
    specRef: "vwp-v1 §6.7, 09-ui §6",
    ...(isGroundTruthOverlayName(name)
      ? { caveat: "Ground truth: no radio in the simulation can see this, so it has to be switched off for an evaluation that must not cheat." }
      : {}),
  };
}

/** What each `vwp-world/1` summary figure counts, by the key the world summary uses (§4). */
export const WORLD_FIELDS: Readonly<Record<string, { readonly label: string; readonly section: string }>> = {
  lanes: { label: "lanes", section: "lane geometry" },
  buildings: { label: "buildings", section: "building outlines" },
  junctions: { label: "junctions", section: "junction table" },
  signals: { label: "signal heads", section: "signal table" },
  sites: { label: "roadside units", section: "roadside-unit table" },
  crossings: { label: "crossings", section: "crossing table" },
  landuse: { label: "land-use polygons", section: "land-use table" },
  bytes: { label: "payload size", section: "file header" },
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
      computation: `Counted from the ${known?.section ?? "contents"} of the world file this run is drawn from.`,
      inputs: "The world file the engine served, checked against the digest the run promised before anything was drawn from it.",
      reference: "The world file format: one section per kind of geometry, each with its own count.",
      specRef: "vwp-v1 §4",
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
      inputs: "The channel each event frame names, resolved through the channel list the run published when it opened.",
      reference: "Events travel on named channels, and a channel delivers nothing until the page subscribes to it.",
      specRef: "vwp-v1 §3.6, §6.12",
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
