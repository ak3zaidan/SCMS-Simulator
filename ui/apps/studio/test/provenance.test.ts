/**
 * The explainability surface.
 *
 * "Every number resolves to what produced it" is the project's first hard rule, and the way it fails
 * is not a crash: it is one readout that was added without a provenance entry, or an entry whose
 * citation drifted from the thing it cites. Both are testable.
 *
 * The overlay-channel check is the one worth having. `OVERLAY_SOURCES` mirrors `overlay_channels`
 * in `crates/v2xw-server/src/rpc.rs`, which is what the engine reports as `needs_channels` in
 * `overlay.set {list: true}` (§6.7). If those two disagree, the "why" panel tells a user to
 * subscribe a channel that will not fill the overlay.
 */

import { describe, expect, it } from "vitest";

import { OVERLAY_NAMES } from "@vwp/protocol";

import {
  CLIENT_PROVENANCE,
  OVERLAY_SOURCES,
  WORLD_FIELDS,
  channelSubject,
  clientProvenance,
  clientSubject,
  differenceSubject,
  eventSubject,
  isGroundTruthOverlayName,
  metricSubject,
  overlayProvenance,
  overlaySubject,
  worldSubject,
} from "../src/lib/provenance.js";

/**
 * `overlay_channels` in `crates/v2xw-server/src/rpc.rs`, transcribed.
 *
 * Every other overlay is `Vec::new()` there — drawn from the world payload or the pose buffer, with
 * no event subscription needed.
 */
const ENGINE_OVERLAY_CHANNELS: Record<string, readonly string[]> = {
  tx_pulses: ["node.tx"],
  links: ["phy.rx"],
  cbr_heatmap: ["mac.cbr"],
  detections: ["det.observation"],
  revoked: ["proto.revocation"],
  reported: ["proto.revocation"],
  attackers_gt: ["gt.kinematics"],
  trajectories_gt: ["gt.kinematics"],
  belief_vs_truth_gt: ["gt.kinematics"],
};

describe("CLIENT_PROVENANCE", () => {
  it("gives every entry a producer, a computation and its inputs", () => {
    for (const [key, entry] of Object.entries(CLIENT_PROVENANCE)) {
      expect(entry.producer, key).not.toBe("");
      expect(entry.computation, key).not.toBe("");
      expect(entry.inputs, key).not.toBe("");
      // A computation that does not end in a full stop is a fragment, and a fragment is where an
      // entry stops explaining and starts labelling.
      expect(entry.computation.endsWith("."), key).toBe(true);
    }
  });

  it("names a package or a crate as the producer, never just 'the browser'", () => {
    for (const [key, entry] of Object.entries(CLIENT_PROVENANCE)) {
      expect(entry.producer, key).toMatch(/^(@vwp\/|v2xw-)/);
    }
  });

  it("covers every figure the readouts open", () => {
    // The keys the components pass to `clientSubject`. A readout added without an entry shows an
    // empty card, which is the quiet failure this list prevents.
    const used = [
      "fps", "frameMs", "p95Ms", "drawCalls", "actorInstances", "actorLive",
      "frames_received", "replay_position", "compare_time", "compare_delta",
    ];
    for (const key of used) expect(clientProvenance(key), key).not.toBeNull();
  });

  it("returns null for an unknown key rather than an empty record", () => {
    expect(clientProvenance("no_such_readout")).toBeNull();
  });
});

describe("OVERLAY_SOURCES", () => {
  it("covers every overlay name §6.7 defines, and nothing else", () => {
    expect(Object.keys(OVERLAY_SOURCES).sort()).toEqual([...OVERLAY_NAMES].sort());
  });

  it("agrees with the engine's needs_channels mapping", () => {
    for (const name of OVERLAY_NAMES) {
      const expected = ENGINE_OVERLAY_CHANNELS[name] ?? [];
      expect([...OVERLAY_SOURCES[name].channels], name).toEqual([...expected]);
    }
  });

  it("gives every overlay a summary of what it draws", () => {
    for (const name of OVERLAY_NAMES) {
      expect(OVERLAY_SOURCES[name].summary, name).not.toBe("");
      expect(OVERLAY_SOURCES[name].summary.endsWith("."), name).toBe(true);
    }
  });
});

describe("overlayProvenance", () => {
  it("names the feeding channels and the subscription rule for an event-fed overlay", () => {
    const prov = overlayProvenance("tx_pulses");
    expect(prov.inputs).toContain("node.tx");
    // The reason an enabled overlay can be empty; without this the pane just looks broken.
    expect(prov.inputs).toContain("§6.12");
  });

  it("says a world-fed overlay needs no subscription", () => {
    const prov = overlayProvenance("lane_markings");
    expect(prov.inputs).toContain("no event subscription");
  });

  it("carries the blind-evaluation caveat for a ground-truth overlay, and only for those", () => {
    expect(overlayProvenance("attackers_gt").caveat).toContain("Ground truth");
    expect(overlayProvenance("reported").caveat).toBeUndefined();
  });

  it("does not invent a description for an overlay this build has never heard of", () => {
    // The engine's catalogue is authoritative and may list an overlay a newer server added.
    const prov = overlayProvenance("some_future_overlay");
    expect(prov.computation).toContain("no description");
  });
});

describe("isGroundTruthOverlayName", () => {
  it("matches §6.7's `_gt` suffix rule", () => {
    const fromSuffix = OVERLAY_NAMES.filter((n) => isGroundTruthOverlayName(n));
    expect([...fromSuffix].sort()).toEqual(["attackers_gt", "belief_vs_truth_gt", "trajectories_gt"]);
  });
});

describe("WORLD_FIELDS", () => {
  it("cites a section for every figure the world summary shows", () => {
    for (const [key, entry] of Object.entries(WORLD_FIELDS)) {
      expect(entry.label, key).not.toBe("");
      expect(entry.section, key).not.toBe("");
    }
  });
});

describe("the subject builders", () => {
  it("omits an absent value rather than writing 'null' into it", () => {
    const subject = metricSubject("pdr", null);
    expect(subject).toEqual({ kind: "metric", id: "pdr", label: "pdr" });
    expect("value" in subject).toBe(false);
    expect("unit" in subject).toBe(false);
    expect("provId" in subject).toBe(false);
  });

  it("carries a prov_id when the metric's samples reported one", () => {
    const subject = metricSubject("pdr", 0.9, "ratio", 42);
    expect(subject).toMatchObject({ kind: "metric", id: "pdr", value: "0.9", unit: "ratio", provId: 42 });
  });

  it("treats prov_id 0 as no provenance, which is what §3.7 defines it as", () => {
    expect("provId" in metricSubject("pdr", 0.9, "ratio", 0)).toBe(false);
    expect("provId" in eventSubject("sec.cert", "pseudonym change", 7, 0)).toBe(false);
  });

  it("attaches the client record to a client-measured subject", () => {
    const subject = clientSubject("fps", "frames per second", "58", "1/s");
    expect(subject.kind).toBe("entity");
    expect(subject.client?.producer).toContain("FrameStats");
  });

  it("leaves the client record off a subject with no entry, so the gap is visible", () => {
    expect(clientSubject("no_such_readout", "mystery").client).toBeUndefined();
  });

  it("builds an overlay subject that says whether the overlay is on", () => {
    expect(overlaySubject("links", true).value).toBe("on");
    expect(overlaySubject("links", false).value).toBe("off");
    expect(overlaySubject("links", true).kind).toBe("overlay");
  });

  it("builds a world subject that cites the section that counted the figure", () => {
    const subject = worldSubject("lanes", "4,312");
    expect(subject.kind).toBe("world");
    expect(subject.client?.computation).toContain("§4.3");
    // §10.5 W3 is what makes the figure trustworthy at all, so the card names it.
    expect(subject.client?.inputs).toContain("W3");
  });

  it("builds a channel subject", () => {
    const subject = channelSubject("node.tx", "1,024 frames decoded");
    expect(subject).toMatchObject({ kind: "channel", id: "node.tx", value: "1,024 frames decoded" });
    expect(subject.client?.computation).toContain("node.tx");
  });

  it("builds an event subject that keeps the node and the prov_id", () => {
    const subject = eventSubject("det.observation", "detection score 0.91", 12, 7);
    expect(subject).toMatchObject({ kind: "event", id: "det.observation", node: 12, provId: 7 });
  });

  it("builds a difference subject that states the difference was computed here", () => {
    const subject = differenceSubject("pdr", -0.2, "ratio");
    expect(subject.kind).toBe("metric");
    expect(subject.label).toContain("B − A");
    expect(subject.client?.producer).toContain("CompareController");
    expect(subject.client?.caveat).toContain("definition is not compared");
  });

  it("omits the difference when there is none to state", () => {
    expect("value" in differenceSubject("pdr", null)).toBe(false);
  });
});
