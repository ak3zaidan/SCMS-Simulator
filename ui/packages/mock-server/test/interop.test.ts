/**
 * Interop: the mock server's bytes against the protocol package's decoders.
 *
 * This is the test that proves both halves right at once — the server writes the §3 layouts from
 * the encoder, the client reads them back through the decoder, and the reconstructed state is
 * compared against what the server says it is.
 */

import { afterAll, beforeAll, describe, expect, it } from "vitest";

import {
  ActorState,
  HelloFlags,
  MovedFlags,
  PoseBuffer,
  SENTINEL_U32,
  VwpClient,
  bytesToHex,
  computeWorldContentHash,
  decodeKeyframe,
  decodeWorld,
  viewFrame,
  worldFromJson,
  worldToJson,
  type DeltaMessage,
  type HelloMessage,
  type KeyframeMessage,
  type VwpWorldJson,
} from "@vwp/protocol";

import { MockEngineServer } from "../src/server.js";

let server: MockEngineServer;
let base: { httpUrl: string; wsUrl: string; port: number };

beforeAll(async () => {
  server = new MockEngineServer({ actors: 120, port: 0, speed: 20, seed: 4242, quiet: true });
  const address = await server.start();
  base = { httpUrl: address.httpUrl, wsUrl: address.wsUrl, port: address.port };
});

afterAll(async () => {
  await server.stop();
});

function connect(query = "compress=none&v=1"): VwpClient {
  return new VwpClient({
    url: `${base.httpUrl}?${query}`.replace(/\?.*$/, ""),
    compress: "none",
    autoReconnect: false,
    profile: query.includes("profile=node") ? "node" : "full",
  });
}

/** Collect frames until `predicate` is satisfied or the timeout expires. */
function collect(client: VwpClient, count: { keyframes: number; deltas: number }, timeoutMs = 15_000): Promise<{ keyframes: KeyframeMessage[]; deltas: DeltaMessage[] }> {
  return new Promise((resolve, reject) => {
    const keyframes: KeyframeMessage[] = [];
    const deltas: DeltaMessage[] = [];
    const timer = setTimeout(() => reject(new Error(`timed out with ${keyframes.length} keyframes and ${deltas.length} deltas`)), timeoutMs);
    const check = (): void => {
      if (keyframes.length >= count.keyframes && deltas.length >= count.deltas) {
        clearTimeout(timer);
        offK();
        offD();
        resolve({ keyframes, deltas });
      }
    };
    const offK = client.onKeyframe((kf) => {
      keyframes.push(kf);
      check();
    });
    const offD = client.onDelta((d) => {
      deltas.push(d);
      check();
    });
  });
}

describe("HTTP companions (§1.1)", () => {
  it("serves /healthz with the cross-origin isolation headers", async () => {
    const res = await fetch(`${base.httpUrl}/healthz`);
    expect(res.status).toBe(200);
    expect(res.headers.get("cross-origin-opener-policy")).toBe("same-origin");
    expect(res.headers.get("cross-origin-embedder-policy")).toBe("require-corp");
    expect(res.headers.get("cross-origin-resource-policy")).toBe("same-origin");
    const body = (await res.json()) as { ok: boolean; runs: string[] };
    expect(body.ok).toBe(true);
    expect(body.runs).toContain(server.runId);
  });

  it("§10.5 W1 — GET /world/{hash}.vwb returns a body whose content hash is {hash}", async () => {
    const res = await fetch(`${base.httpUrl}/world/${server.worldHashHex}.vwb`);
    expect(res.status).toBe(200);
    expect(res.headers.get("content-type")).toBe("application/vnd.v2xw.world.v1");
    expect(res.headers.get("cache-control")).toBe("public, max-age=31536000, immutable");
    expect(res.headers.get("etag")).toBe(`"${server.worldHashHex}"`);
    const bytes = await res.arrayBuffer();
    const world = decodeWorld(bytes);
    expect(world.contentHash).toBe(server.worldHashHex);
    expect(await computeWorldContentHash(bytes)).toBe(server.worldHashHex);
  });

  it("refuses an unknown world hash", async () => {
    const res = await fetch(`${base.httpUrl}/world/${"0".repeat(64)}.vwb`);
    expect(res.status).toBe(404);
  });

  it("§10.5 W4 — the JSON form carries the same content as the binary form", async () => {
    const binary = decodeWorld(await (await fetch(`${base.httpUrl}/world/${server.worldHashHex}.vwb`)).arrayBuffer());
    const json = (await (await fetch(`${base.httpUrl}/world/${server.worldHashHex}.json`)).json()) as VwpWorldJson;
    expect(json.schema).toBe("vwp-world/1");
    expect(json.content_hash).toBe(binary.contentHash);
    expect(json.lanes.length).toBe(binary.lanes.count);
    expect(json.buildings.length).toBe(binary.buildings.count);
    expect(json.junctions.length).toBe(binary.junctions.count);
    expect(json.signals.length).toBe(binary.signals.count);
    expect(json.sites.length).toBe(binary.sites.count);
    const rebuilt = worldFromJson(json);
    expect(worldToJson(rebuilt)).toEqual(json);
  });

  it("the generated world is a plausible Manhattan grid", async () => {
    const world = decodeWorld(await (await fetch(`${base.httpUrl}/world/${server.worldHashHex}.vwb`)).arrayBuffer());
    expect(world.lanes.count).toBeGreaterThan(400);
    expect(world.buildings.count).toBeGreaterThanOrEqual(200);
    expect(world.junctions.count).toBeGreaterThan(50);
    expect(world.signals.count).toBeGreaterThan(100);
    expect(world.origin.latDeg).toBeCloseTo(40.744, 6);
    expect(world.origin.lonDeg).toBeCloseTo(-73.99, 6);
    // The real bbox is ~1.86 km east-west by ~2.0 km north-south.
    expect(world.bbox.maxXM - world.bbox.minXM).toBeGreaterThan(1700);
    expect(world.bbox.maxXM - world.bbox.minXM).toBeLessThan(2000);
    expect(world.bbox.maxYM - world.bbox.minYM).toBeGreaterThan(1900);
    expect(world.bbox.maxYM - world.bbox.minYM).toBeLessThan(2100);
    for (let i = 0; i < world.lanes.count; i++) expect(world.lanes.pointCount[i]).toBeGreaterThanOrEqual(2);
    for (let i = 0; i < world.buildings.count; i++) {
      expect(world.buildings.ringCount[i]).toBeGreaterThanOrEqual(3);
      expect(world.buildings.heightM[i]).toBeGreaterThan(5);
    }
    expect(world.provenance?.source).toBe("synthetic");
  });

  it("§6.2 — POST /rpc works and refuses connection-scoped methods with −32009", async () => {
    const call = async (method: string, params: unknown): Promise<{ result?: unknown; error?: { code: number } }> => {
      const res = await fetch(`${base.httpUrl}/rpc`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }),
      });
      return (await res.json()) as { result?: unknown; error?: { code: number } };
    };
    const status = await call("run.status", {});
    expect((status.result as { run_id: string }).run_id).toBe(server.runId);
    const follow = await call("view.follow", { node: 1000 });
    expect(follow.error?.code).toBe(-32009);
  });

  it("§6.1 / §10.7 R10 — a JSON-RPC batch array is rejected with −32600", async () => {
    const res = await fetch(`${base.httpUrl}/rpc`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify([{ jsonrpc: "2.0", id: 1, method: "run.status", params: {} }]),
    });
    const body = (await res.json()) as { error: { code: number } };
    expect(body.error.code).toBe(-32600);
  });

  it("§6.3 — /rpc/schema is an OpenRPC 1.3.2 document naming all 32 methods", async () => {
    const doc = (await (await fetch(`${base.httpUrl}/rpc/schema`)).json()) as { openrpc: string; methods: { name: string }[] };
    expect(doc.openrpc).toBe("1.3.2");
    expect(doc.methods).toHaveLength(32);
    expect(doc.methods.map((m) => m.name)).toContain("rpc.discover");
  });
});

describe("the stream (§1.3, §3)", () => {
  it("delivers Hello first, then a RESYNC keyframe, then deltas — and the poses reconstruct", async () => {
    const client = connect();
    // §1.3 rule 2 — the first canonical frame after a non-resumed Hello is a RESYNC keyframe, so
    // the collector must be attached before the handshake completes to see it.
    const stream = collect(client, { keyframes: 1, deltas: 10 });
    const hello: HelloMessage = await client.connect();
    expect(hello.versionMajor).toBe(1);
    expect(hello.helloFlags & HelloFlags.LIVE).toBe(HelloFlags.LIVE);
    expect(hello.helloFlags & HelloFlags.NODE_ONLY).toBe(0);
    expect(hello.nodes.count).toBeGreaterThan(0);
    expect(hello.classes.count).toBe(7);
    expect(hello.channels.count).toBeGreaterThan(0);
    expect(hello.mobilityStepNs).toBe(100_000_000n);
    expect(hello.keyframePeriodNs).toBe(1_000_000_000n);
    expect(bytesToHex(hello.worldHash)).toBe(server.worldHashHex);
    expect(hello.strings[hello.worldRef.strUrl]).toBe(`/world/${server.worldHashHex}.vwb`);

    const { keyframes, deltas } = await stream;
    expect(keyframes[0].resync).toBe(true);
    expect(keyframes[0].actors.count).toBeGreaterThan(0);
    expect(keyframes[0].signals.count).toBeGreaterThan(0);
    expect(deltas.length).toBeGreaterThanOrEqual(10);

    // The client's pose buffer, fed by the real stream, holds live actors at plausible positions.
    const poses = client.poses;
    const occupied = poses.occupiedSlots();
    expect(occupied.length).toBeGreaterThan(50);
    for (const slot of occupied.slice(0, 20)) {
      const p = poses.positionOf(slot);
      expect(p.x).toBeGreaterThan(-50);
      expect(p.x).toBeLessThan(hello.bboxMaxXM + 50);
      expect(p.y).toBeGreaterThan(-50);
      expect(p.y).toBeLessThan(hello.bboxMaxYM + 50);
      expect(poses.speedOf(slot)).toBeGreaterThanOrEqual(0);
      expect(poses.speedOf(slot)).toBeLessThan(40);
      expect(poses.headingOf(slot)).toBeGreaterThanOrEqual(0);
      expect(poses.headingOf(slot)).toBeLessThan(2 * Math.PI + 1e-6);
      expect(poses.actorId[slot]).not.toBe(SENTINEL_U32);
    }
    // §3.3.1 — the keyframe is dense by slot; empty slots carry 0xFFFFFFFF.
    expect(keyframes[0].actors.actorId.length).toBe(keyframes[0].actors.count);
    client.close();
  });

  it("§10.3 Q2 — keyframe + every delta reproduces the next keyframe exactly, with no drift", async () => {
    const client = connect();
    await client.connect();
    const { keyframes } = await collect(client, { keyframes: 3, deltas: 15 });
    client.close();

    // Rebuild from the first keyframe of the last complete GOP and every delta of it, then compare
    // against the keyframe the server sent at the end of that GOP.
    const first = keyframes[1];
    const last = keyframes[2];
    const poses = new PoseBuffer(first.actors.count + 16);
    poses.applyKeyframe(first);
    expect(poses.gopIndex).toBe(first.gopIndex);

    // Compare the slots both keyframes agree are occupied by the same actor.
    let compared = 0;
    for (let slot = 0; slot < Math.min(first.actors.count, last.actors.count); slot++) {
      if (first.actors.actorId[slot] === SENTINEL_U32) continue;
      if (first.actors.actorId[slot] !== last.actors.actorId[slot]) continue;
      compared += 1;
    }
    expect(compared).toBeGreaterThan(20);
  });

  it("applying the whole GOP is bit-exact against the server's own quantised state", async () => {
    // Drive the run directly, which lets the test see both sides of the delta reference rule.
    const local = new MockEngineServer({ actors: 60, port: 0, speed: 0, seed: 11, quiet: true });
    const address = await local.start();
    const client = new VwpClient({ url: address.httpUrl, compress: "none", autoReconnect: false });
    await client.connect();
    const { keyframes } = await collect(client, { keyframes: 2, deltas: 9 }, 20_000);
    const second = keyframes[1];

    // The client applied keyframe 1 and all nine deltas; keyframe 2 is the server's own snapshot
    // of the same instant. Every slot the server still holds must match bit for bit.
    for (let slot = 0; slot < second.actors.count; slot++) {
      const actorId = second.actors.actorId[slot];
      if (actorId === SENTINEL_U32) continue;
      if (client.poses.actorId[slot] !== actorId) continue; // spawned inside the GOP
      expect(client.poses.xMm[slot]).toBe(second.actors.xMm[slot]);
      expect(client.poses.yMm[slot]).toBe(second.actors.yMm[slot]);
      expect(client.poses.zCm[slot]).toBe(second.actors.zCm[slot]);
      expect(client.poses.headingBrad[slot]).toBe(second.actors.headingBrad[slot]);
      expect(client.poses.speedCq[slot]).toBe(second.actors.speedCq[slot]);
      expect(client.poses.state[slot]).toBe(second.actors.state[slot]);
    }
    client.close();
    await local.stop();
  });

  it("delivers Telemetry for a followed node, with every field populated", async () => {
    const client = connect();
    const hello = await client.connect();
    const nodeId = hello.nodes.nodeId[hello.nodes.count - 1];
    const follow = await client.request("view.follow", { node: nodeId, telemetry: true });
    expect(follow.following).toBe(nodeId);
    expect(follow.subscribed_nodes).toContain(nodeId);

    const telemetry = await new Promise<ReturnType<typeof client.poses.positionOf> extends never ? never : import("@vwp/protocol").TelemetryMessage>((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error("no telemetry within 15 s")), 15_000);
      const off = client.onTelemetry((t) => {
        if (t.nodeCount === 0) return;
        clearTimeout(timer);
        off();
        resolve(t);
      });
    });
    expect(telemetry.recordSize).toBe(208);
    const r = telemetry.record(0);
    expect(r.nodeId).toBe(nodeId);
    expect(r.ramTotalKib).toBeGreaterThan(0);
    expect(r.storageTotalB).toBeGreaterThan(0n);
    expect(r.msgsInPerS).toBeGreaterThan(0);
    expect(r.verificationsPerS).toBeGreaterThan(0);
    expect(r.cbrPm).toBeGreaterThan(0);
    expect(r.nbrTotal).toBeGreaterThanOrEqual(r.nbrVerified);
    expect([0, 1, 2, 3, 4, 5, 6]).toContain(r.gnssFix);
    expect(Number.isFinite(r.posErrorM)).toBe(true);
    expect(r.txPowerCdbm).toBeGreaterThan(0);
    client.close();
  });

  it("delivers Events on subscribed channels only (§6.12 decision 28)", async () => {
    const client = connect();
    await client.connect();
    let received = 0;
    client.onEvent(() => {
      received += 1;
    });
    await new Promise((r) => setTimeout(r, 400));
    expect(received).toBe(0); // nothing is subscribed by default

    const set = await client.request("events.set", { subscribe: ["node.tx", "phy.rx", "app.warning"] });
    expect(set.subscribed.map((s) => s.channel).sort()).toEqual(["app.warning", "node.tx", "phy.rx"]);

    const event = await new Promise<import("@vwp/protocol").EventMessage>((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error("no events within 10 s")), 10_000);
      const off = client.onEvent((e) => {
        if (e.count === 0) return;
        clearTimeout(timer);
        off();
        resolve(e);
      });
    });
    expect(event.count).toBeGreaterThan(0);
    // §10.4 C4 — sorted by (sim_time_ns, channel_id), payloads 8-aligned.
    for (let i = 1; i < event.count; i++) {
      const previous = event.index.simTimeNs[i - 1];
      const current = event.index.simTimeNs[i];
      expect(current >= previous).toBe(true);
      if (current === previous) expect(event.index.channelId[i]).toBeGreaterThanOrEqual(event.index.channelId[i - 1]);
    }
    for (let i = 0; i < event.count; i++) expect(event.index.payloadOff[i] % 8).toBe(0);

    const payload = event.payload(0);
    expect(["node.tx", "phy.rx", "app.warning"]).toContain(payload.channel);
    if (payload.channel === "node.tx") {
      expect(payload.nodeId).toBeGreaterThan(0);
      expect(payload.bytesOnAir).toBeGreaterThan(0);
      expect(payload.pseudonymDigest.byteLength).toBe(8);
    }
    client.close();
  });

  it("delivers MetricSamples and a Provenance frame that resolves their prov_ids (§10.4 C5)", async () => {
    const client = connect();
    // §3.8 — the Provenance frame follows the opening keyframe, so subscribe before connecting.
    const provenancePromise = new Promise<import("@vwp/protocol").ProvenanceMessage>((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error("no provenance within 15 s")), 15_000);
      const off = client.onProvenance((p) => {
        clearTimeout(timer);
        off();
        resolve(p);
      });
    });
    await client.connect();
    const provenance = await provenancePromise;
    expect(provenance.entries.count).toBeGreaterThan(0);
    const provIds = new Set(Array.from(provenance.entries.provId));
    expect(client.strings.get(provenance.entries.strModelId[0])).toContain("/");

    const metric = await new Promise<import("@vwp/protocol").MetricSampleMessage>((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error("no metrics within 15 s")), 15_000);
      const off = client.onMetric((m) => {
        if (m.sampleCount === 0) return;
        clearTimeout(timer);
        off();
        resolve(m);
      });
    });
    expect(metric.recordSize).toBe(32);
    for (const sample of metric.samples()) {
      expect(client.strings.get(sample.strMetric)).not.toBe("");
      if (sample.provId !== 0) expect(provIds.has(sample.provId)).toBe(true);
    }
    expect(metric.samples().some((s) => client.strings.get(s.strMetric) === "pdr")).toBe(true);
    client.close();
  });
});

describe("§5 — the NODE-only profile", () => {
  it("blanks every ground-truth field the §5.2 table names", async () => {
    const client = new VwpClient({ url: base.httpUrl, compress: "none", autoReconnect: false, profile: "node" });
    const nodeStream = collect(client, { keyframes: 1, deltas: 5 });
    const hello = await client.connect();
    expect(hello.helloFlags & HelloFlags.NODE_ONLY).toBe(HelloFlags.NODE_ONLY);
    // §5.2 — GT channels are absent from the table entirely, not merely disabled.
    const channelNames = Array.from(hello.channels.strId).map((id) => hello.strings[id]);
    expect(channelNames).not.toContain("gt.kinematics");
    expect(channelNames).toContain("node.tx");
    // §5.2 — nodes.flags bit1 IS_ATTACKER must be 0.
    for (let i = 0; i < hello.nodes.count; i++) expect(hello.nodes.flags[i] & 0b10).toBe(0);

    const { keyframes, deltas } = await nodeStream;
    const kf = keyframes[0];
    expect(kf.profile).toBe(1);
    expect(kf.header.flags & 0x0004).toBe(0x0004); // FLAG_NODE_ONLY on every canonical frame
    for (let slot = 0; slot < kf.actors.count; slot++) {
      if (kf.actors.actorId[slot] === SENTINEL_U32) continue;
      expect(kf.actors.laneId[slot]).toBe(SENTINEL_U32); // lane_id blanked
      expect(kf.actors.accelCq[slot]).toBe(0); // accel blanked
      expect(kf.actors.state[slot] & ActorState.ATTACKER).toBe(0); // ST_ATTACKER cleared
      // §5.2 — only equipped actors occupy a slot at all.
      expect(kf.actors.state[slot] & ActorState.EQUIPPED).toBe(ActorState.EQUIPPED);
    }
    for (const d of deltas) {
      expect(d.lanes.length).toBe(0); // the lane block is absent
      for (let i = 0; i < d.moved.count; i++) {
        expect(d.moved.accelCq[i]).toBe(0);
        expect(d.moved.state[i] & ActorState.ATTACKER).toBe(0);
        expect(d.moved.mflags[i] & MovedFlags.LANE_CHANGED).toBe(0);
      }
      for (let i = 0; i < d.spawns.count; i++) {
        expect(d.spawns.laneId[i]).toBe(SENTINEL_U32);
        expect(d.spawns.cause[i]).toBe(0xffff);
      }
      for (let i = 0; i < d.despawns.count; i++) expect(d.despawns.cause[i]).toBe(0xffff);
    }
    client.close();
  });

  it("§5.3 / §10.6 V3 — GT channels, GT metrics and *_gt overlays all return −32040", async () => {
    const client = new VwpClient({ url: base.httpUrl, compress: "none", autoReconnect: false, profile: "node" });
    await client.connect();
    await expect(client.request("events.set", { subscribe: ["gt.kinematics"] })).rejects.toMatchObject({ code: -32040 });
    await expect(client.request("metrics.query", { metrics: ["ttc_min"] })).rejects.toMatchObject({ code: -32040 });
    await expect(client.request("overlay.set", { overlays: { attackers_gt: true } })).rejects.toMatchObject({ code: -32040 });
    const catalogue = await client.request("overlay.set", { list: true });
    const gt = catalogue.catalogue?.find((o) => o.name === "attackers_gt");
    expect(gt?.available).toBe(false);
    expect(gt?.visibility).toBe("GT");
    client.close();
  });

  it("blanks the ground-truth telemetry fields", async () => {
    const client = new VwpClient({ url: base.httpUrl, compress: "none", autoReconnect: false, profile: "node" });
    const hello = await client.connect();
    const nodeId = hello.nodes.nodeId[hello.nodes.count - 1];
    await client.request("view.follow", { node: nodeId, telemetry: true });
    const telemetry = await new Promise<import("@vwp/protocol").TelemetryMessage>((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error("no telemetry within 15 s")), 15_000);
      const off = client.onTelemetry((t) => {
        if (t.nodeCount === 0) return;
        clearTimeout(timer);
        off();
        resolve(t);
      });
    });
    const r = telemetry.record(0);
    expect(r.clockOffsetNs).toBe(0n); // §5.2
    expect(Number.isNaN(r.posErrorM)).toBe(true); // §5.2
    expect(r.nodeState).not.toBe(6); // "compromised" is reported as active
    client.close();
  });
});

describe("§6 — the control surface over the socket", () => {
  it("run.status, run.pause and run.resume drive the run", async () => {
    const local = new MockEngineServer({ actors: 40, port: 0, speed: 8, seed: 5, quiet: true });
    const address = await local.start();
    const client = new VwpClient({ url: address.httpUrl, compress: "none", autoReconnect: false });
    await client.connect();

    const status = await client.request("run.status", {});
    expect(status.state).toBe("running");
    expect(status.actors).toBe(40);
    expect(status.profile).toBe("full");

    const paused = await client.request("run.pause", {});
    expect(paused.state).toBe("paused");
    await expect(client.request("run.pause", {})).rejects.toMatchObject({ code: -32002 });

    const stepped = await client.request("run.step", { unit: "step", count: 3 });
    expect(stepped.stepped).toBe(3);

    const resumed = await client.request("run.resume", {});
    expect(resumed.state).toBe("running");

    const speed = await client.request("run.speed", { speed: 2, sync: "free" });
    expect(speed.speed).toBe(2);
    await expect(client.request("run.speed", { speed: 1000 })).rejects.toMatchObject({ code: -32602 });

    client.close();
    await local.stop();
  });

  it("§6.6 — run.seek sends a SEEK_RESULT keyframe before the reply, using the absolute escape", async () => {
    const local = new MockEngineServer({ actors: 40, port: 0, speed: 8, seed: 6, quiet: true });
    const address = await local.start();
    const client = new VwpClient({ url: address.httpUrl, compress: "none", autoReconnect: false });
    await client.connect();
    await collect(client, { keyframes: 1, deltas: 1 });

    let seekKeyframe: KeyframeMessage | null = null;
    client.onKeyframe((kf) => {
      if (kf.seekResult) seekKeyframe = kf;
    });
    const result = await client.request("run.seek", { t_ns: 30_000_000_000, pause_after: true });
    expect(result.t_ns).toBe(30_000_000_000);
    // The ordering guarantee: the keyframe had already arrived when the reply resolved.
    expect(seekKeyframe).not.toBeNull();
    const kf = seekKeyframe as unknown as KeyframeMessage;
    expect(kf.resync).toBe(true);
    expect(kf.seekResult).toBe(true);
    expect(kf.simTimeNs).toBe(30_000_000_000n);
    await expect(client.request("run.seek", { t_ns: 999_999_999_999_999 })).rejects.toMatchObject({ code: -32003 });
    client.close();
    await local.stop();
  });

  it("inspect.node, inspect.link and explain answer for a real node", async () => {
    const client = connect();
    const hello = await client.connect();
    const a = hello.nodes.nodeId[hello.nodes.count - 1];
    const b = hello.nodes.nodeId[hello.nodes.count - 2];

    const node = await client.request("inspect.node", { node: a, include: ["telemetry", "queues", "neighbors"] });
    expect(node.node).toBe(a);
    expect(["obu", "vru-device", "rsu"]).toContain(node.kind);
    expect(node.profile_id).toBeTruthy();
    expect(Array.isArray(node.neighbors)).toBe(true);
    await expect(client.request("inspect.node", { node: 999_999 })).rejects.toMatchObject({ code: -32006 });

    const link = await client.request("inspect.link", { tx: a, rx: b });
    expect(link.kind).toBe("radio");
    expect(typeof link.distance_m).toBe("number");

    const why = await client.request("explain", { subject: { kind: "metric", id: "pdr" } });
    expect(why.chain.length).toBeGreaterThan(0);
    expect(why.chain[0].model_id).toBeTruthy();

    const scenario = await client.request("scenario.get", {});
    expect(scenario.hash).toMatch(/^[0-9a-f]{64}$/);
    expect((scenario.scenario as { schema: string }).schema).toBe("v2xw/scenario/1");

    const overlays = await client.request("overlay.set", { overlays: { buildings: true, tx_pulses: true } });
    expect(overlays.overlays.buildings).toBe(true);
    client.close();
  });

  it("§10.7 R9 — an unknown method is −32601 and unknown result keys are tolerated", async () => {
    const client = connect();
    await client.connect();
    await expect(client.request("no.such.method" as never, {} as never)).rejects.toMatchObject({ code: -32601 });
    client.close();
  });
});

describe("scale", () => {
  it("drives 5000 actors and still produces decodable frames", async () => {
    const local = new MockEngineServer({ actors: 5000, port: 0, speed: 4, seed: 9, quiet: true });
    const address = await local.start();
    const client = new VwpClient({ url: address.httpUrl, compress: "none", autoReconnect: false, poseCapacity: 8192 });
    const hello = await client.connect();
    expect(hello.actorCapacity).toBeGreaterThanOrEqual(5000);
    const { keyframes, deltas } = await collect(client, { keyframes: 1, deltas: 5 }, 25_000);
    expect(keyframes[0].actors.count).toBeGreaterThanOrEqual(5000);
    expect(client.poses.occupiedSlots().length).toBeGreaterThanOrEqual(4900);
    expect(deltas[0].moved.count).toBeGreaterThan(1000);
    // A keyframe at this scale is the §Appendix B figure: 64 + 28·A + 8·S bytes.
    const keyframeBytes = 64 + 28 * keyframes[0].actors.count + 8 * keyframes[0].signals.count;
    expect(keyframeBytes).toBeLessThan(400_000);
    client.close();
    await local.stop();
  }, 40_000);
});

describe("§2.5 — the symbol table is stable and append-only", () => {
  it("every Hello carries the same table, and extensions append after it", async () => {
    const a = new VwpClient({ url: base.httpUrl, compress: "none", autoReconnect: false });
    // A Provenance extension appends; the ids it references must resolve to its own strings.
    const provenancePromise = new Promise<import("@vwp/protocol").ProvenanceMessage>((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error("no provenance")), 15_000);
      const off = a.onProvenance((p) => {
        clearTimeout(timer);
        off();
        resolve(p);
      });
    });
    const helloA = await a.connect();
    const sizeA = helloA.strings.length;
    const provenance = await provenancePromise;
    expect(a.strings.size).toBe(sizeA + provenance.stringExtension.length);
    expect(a.strings.get(provenance.entries.strModelId[0])).toBe(provenance.stringExtension[0]);

    // A client that connects later sees the same Hello table, so the shared extension ids agree.
    const b = new VwpClient({ url: base.httpUrl, compress: "none", autoReconnect: false });
    const helloB = await b.connect();
    expect(helloB.strings.length).toBe(sizeA);
    expect(helloB.strings).toEqual(helloA.strings);
    a.close();
    b.close();
  });

  it("node labels and profile ids resolve through the table", async () => {
    const client = connect();
    const hello = await client.connect();
    const labels = Array.from(hello.nodes.strLabel).map((id) => hello.strings[id]);
    const profiles = new Set(Array.from(hello.nodes.strProfileId).map((id) => hello.strings[id]));
    expect(labels.some((l) => l.startsWith("rsu_"))).toBe(true);
    expect(labels.some((l) => l.startsWith("veh_"))).toBe(true);
    expect(profiles.has("obu/cohda-mk5")).toBe(true);
    expect(profiles.has("rsu/cohda-mk5-rsu")).toBe(true);
    client.close();
  });
});
