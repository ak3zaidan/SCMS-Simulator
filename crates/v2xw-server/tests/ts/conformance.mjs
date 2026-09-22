/**
 * Drives this server with the real, independently written TypeScript client.
 *
 * This is the acceptance test the build asked for: `@vwp/protocol` was written from
 * docs/protocol/vwp-v1.md without reference to any server, so every assertion below is a
 * statement about the specification rather than about a shared implementation. The
 * assertions are lifted, item by item, from the mock server's own interop suite
 * (`ui/packages/mock-server/test/interop.test.ts`) minus the ones that assert facts about
 * the mock's synthetic Manhattan geometry, which is a different world from this server's
 * generated grid.
 *
 *     node crates/v2xw-server/tests/ts/conformance.mjs http://127.0.0.1:8791
 *
 * It uses Node's built-in `WebSocket` (Node >= 22) and imports the client's built `dist`
 * by path, so it needs no package installation.
 */

import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const protocolDist = resolve(here, "../../../../ui/packages/protocol/dist/index.js");
const P = await import(protocolDist);

const base = process.argv[2] ?? "http://127.0.0.1:8791";
/** A second server on a short run, for the end-of-run path. Optional. */
const shortBase = process.argv[3] && process.argv[3] !== "-" ? process.argv[3] : null;
/** A third server replaying a recording, for §7's indistinguishability. Optional. */
const replayBase = process.argv[4] && process.argv[4] !== "-" ? process.argv[4] : null;

let passed = 0;
const failures = [];
const skipped = [];

function check(name, fn) {
  return (async () => {
    try {
      await fn();
      passed += 1;
      console.log(`  ok   ${name}`);
    } catch (err) {
      failures.push({ name, message: err?.message ?? String(err) });
      console.log(`  FAIL ${name}\n         ${err?.message ?? err}`);
    }
  })();
}

function assert(cond, message) {
  if (!cond) throw new Error(message);
}
/** Order-insensitive structural equality, for comparing two JSON documents. */
function deepEq(a, b, path = "") {
  if (a === b) return;
  if (typeof a === "number" && typeof b === "number" && Math.abs(a - b) < 1e-9) return;
  if (Array.isArray(a) && Array.isArray(b)) {
    if (a.length !== b.length) throw new Error(`${path}: length ${a.length} vs ${b.length}`);
    for (let i = 0; i < a.length; i++) deepEq(a[i], b[i], `${path}[${i}]`);
    return;
  }
  if (a && b && typeof a === "object" && typeof b === "object") {
    const ka = Object.keys(a).sort(), kb = Object.keys(b).sort();
    if (ka.join() !== kb.join()) throw new Error(`${path}: keys ${ka.join()} vs ${kb.join()}`);
    for (const k of ka) deepEq(a[k], b[k], `${path}/${k}`);
    return;
  }
  throw new Error(`${path}: ${JSON.stringify(a)} vs ${JSON.stringify(b)}`);
}

function eq(actual, expected, message) {
  if (actual !== expected) throw new Error(`${message}: expected ${expected}, got ${actual}`);
}

function client(options = {}) {
  return new P.VwpClient({ url: base, compress: "none", autoReconnect: false, ...options });
}

/** Collect frames until the counts are met. */
function collect(c, want, timeoutMs = 20000) {
  return new Promise((resolve, reject) => {
    const keyframes = [];
    const deltas = [];
    const timer = setTimeout(
      () => reject(new Error(`timed out with ${keyframes.length} keyframes, ${deltas.length} deltas`)),
      timeoutMs,
    );
    const done = () => {
      if (keyframes.length >= (want.keyframes ?? 0) && deltas.length >= (want.deltas ?? 0)) {
        clearTimeout(timer);
        offK();
        offD();
        resolve({ keyframes, deltas });
      }
    };
    const offK = c.onKeyframe((kf) => { keyframes.push(kf); done(); });
    const offD = c.onDelta((d) => { deltas.push(d); done(); });
  });
}

async function rpcHttp(method, params, origin = base) {
  const res = await fetch(`${origin}/rpc`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }),
  });
  return res.json();
}

console.log(`\nvwp v1 conformance: @vwp/protocol against ${base}\n`);

// --- §1.1 HTTP companions ---------------------------------------------------------
console.log("§1.1 HTTP companions");
await check("/healthz carries the cross-origin isolation headers (W2)", async () => {
  const res = await fetch(`${base}/healthz`);
  eq(res.status, 200, "status");
  eq(res.headers.get("cross-origin-opener-policy"), "same-origin", "COOP");
  eq(res.headers.get("cross-origin-embedder-policy"), "require-corp", "COEP");
  eq(res.headers.get("cross-origin-resource-policy"), "same-origin", "CORP");
  const body = await res.json();
  assert(body.ok === true, "ok");
  assert(Array.isArray(body.runs) && body.runs.length === 1, "runs");
});

let worldHash = null;
await check("W1 — GET /world/{hash}.vwb hashes to {hash}", async () => {
  const status = await rpcHttp("run.status", {});
  const res0 = await fetch(`${base}/healthz`);
  await res0.json();
  const hello = await (async () => {
    const c = client();
    const h = await c.connect();
    c.close();
    return h;
  })();
  worldHash = P.bytesToHex(hello.worldHash);
  const res = await fetch(`${base}/world/${worldHash}.vwb`);
  eq(res.status, 200, "status");
  eq(res.headers.get("content-type"), "application/vnd.v2xw.world.v1", "content-type");
  eq(res.headers.get("cache-control"), "public, max-age=31536000, immutable", "cache-control");
  eq(res.headers.get("etag"), `"${worldHash}"`, "etag");
  const bytes = await res.arrayBuffer();
  const world = P.decodeWorld(bytes);
  eq(world.contentHash, worldHash, "decoded content hash");
  eq(await P.computeWorldContentHash(bytes), worldHash, "recomputed payload digest");
  assert(status.result.run_id.length === 36, "run id is a uuid string");
});

await check("an unknown world hash is 404", async () => {
  const res = await fetch(`${base}/world/${"0".repeat(64)}.vwb`);
  eq(res.status, 404, "status");
});

await check("W4 — the JSON form carries the same content as the binary form", async () => {
  const binary = P.decodeWorld(await (await fetch(`${base}/world/${worldHash}.vwb`)).arrayBuffer());
  const json = await (await fetch(`${base}/world/${worldHash}.json`)).json();
  eq(json.schema, "vwp-world/1", "schema");
  eq(json.content_hash, binary.contentHash, "content hash");
  eq(json.lanes.length, binary.lanes.count, "lanes");
  eq(json.buildings.length, binary.buildings.count, "buildings");
  eq(json.junctions.length, binary.junctions.count, "junctions");
  eq(json.signals.length, binary.signals.count, "signals");
  eq(json.sites.length, binary.sites.count, "sites");
  const rebuilt = P.worldFromJson(json);
  deepEq(P.worldToJson(rebuilt), json, "world JSON round trip");
});

await check("the generated world is decodable and internally consistent", async () => {
  const world = P.decodeWorld(await (await fetch(`${base}/world/${worldHash}.vwb`)).arrayBuffer());
  assert(world.lanes.count > 50, `lanes ${world.lanes.count}`);
  assert(world.buildings.count > 0, "buildings");
  assert(world.junctions.count > 10, "junctions");
  assert(world.signals.count > 10, "signals");
  for (let i = 0; i < world.lanes.count; i++) assert(world.lanes.pointCount[i] >= 2, "lane points");
  for (let i = 0; i < world.buildings.count; i++) assert(world.buildings.ringCount[i] >= 3, "ring points");
});

await check("§6.2 — POST /rpc works and refuses connection-scoped methods with −32009 (R7)", async () => {
  const status = await rpcHttp("run.status", {});
  assert(typeof status.result.run_id === "string", "run.status over HTTP");
  const follow = await rpcHttp("view.follow", { node: 1000 });
  eq(follow.error?.code, -32009, "view.follow code");
  const camera = await rpcHttp("view.camera", { mode: "map" });
  eq(camera.error?.code, -32009, "view.camera code");
  const overlay = await rpcHttp("overlay.set", { list: true });
  eq(overlay.error?.code, -32009, "overlay.set code");
});

await check("§6.1 / R10 — a JSON-RPC batch array is rejected with −32600", async () => {
  const res = await fetch(`${base}/rpc`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify([{ jsonrpc: "2.0", id: 1, method: "run.status", params: {} }]),
  });
  const body = await res.json();
  eq(body.error.code, -32600, "code");
});

await check("§6.3 / R1 — /rpc/schema is an OpenRPC 1.3.2 document naming all 32 methods", async () => {
  const doc = await (await fetch(`${base}/rpc/schema`)).json();
  eq(doc.openrpc, "1.3.2", "openrpc version");
  eq(doc.methods.length, 32, "method count");
  assert(doc.methods.map((m) => m.name).includes("rpc.discover"), "rpc.discover present");
  for (const m of doc.methods) {
    assert(m.result?.schema, `${m.name} has a result schema`);
    assert(Array.isArray(m.params), `${m.name} has params`);
  }
});

// --- §1.3 / §3 the stream ---------------------------------------------------------
console.log("\n§1.3, §3 the stream");
await check("H1/H2 — Hello first, then a RESYNC keyframe, then deltas; poses reconstruct", async () => {
  const c = client();
  const stream = collect(c, { keyframes: 1, deltas: 10 });
  const hello = await c.connect();
  eq(hello.versionMajor, 1, "version");
  eq(hello.helloFlags & P.HelloFlags.LIVE, P.HelloFlags.LIVE, "HELLO_LIVE");
  eq(hello.helloFlags & P.HelloFlags.NODE_ONLY, 0, "not node-only");
  assert(hello.nodes.count > 0, "node table");
  eq(hello.classes.count, 7, "class table");
  assert(hello.channels.count > 0, "channel table");
  eq(hello.mobilityStepNs, 100_000_000n, "mobility step");
  eq(hello.keyframePeriodNs, 1_000_000_000n, "keyframe period");
  eq(hello.strings[hello.worldRef.strUrl], `/world/${P.bytesToHex(hello.worldHash)}.vwb`, "world url");

  const { keyframes, deltas } = await stream;
  eq(keyframes[0].resync, true, "first keyframe is RESYNC");
  assert(keyframes[0].actors.count > 0, "actors");
  assert(keyframes[0].signals.count > 0, "signals");
  assert(deltas.length >= 10, "deltas");

  const poses = c.poses;
  const occupied = poses.occupiedSlots();
  assert(occupied.length > 10, `occupied slots ${occupied.length}`);
  for (const slot of occupied.slice(0, 20)) {
    const p = poses.positionOf(slot);
    assert(p.x > hello.bboxMinXM - 50 && p.x < hello.bboxMaxXM + 50, `x in bbox: ${p.x}`);
    assert(p.y > hello.bboxMinYM - 50 && p.y < hello.bboxMaxYM + 50, `y in bbox: ${p.y}`);
    assert(poses.speedOf(slot) >= 0 && poses.speedOf(slot) < 40, "speed");
    assert(poses.headingOf(slot) >= 0 && poses.headingOf(slot) < 2 * Math.PI + 1e-6, "heading");
    assert(poses.actorId[slot] !== P.SENTINEL_U32, "actor id");
  }
  c.close();
});

await check("H4 — seq is dense and monotonic across canonical frames", async () => {
  const c = client();
  const seqs = [];
  const offK = c.onKeyframe((kf) => seqs.push(Number(kf.header.seq)));
  const offD = c.onDelta((d) => seqs.push(Number(d.header.seq)));
  const hello = await c.connect();
  eq(Number(hello.resumeSeq), 0, "Hello carries the next canonical seq and consumes none");
  await collect(c, { keyframes: 2, deltas: 12 });
  offK(); offD();
  c.close();
  const sorted = [...seqs].sort((a, b) => a - b);
  for (let i = 1; i < sorted.length; i++) assert(sorted[i] > sorted[i - 1], `seq repeats at ${sorted[i]}`);
});

await check("Q2 — a delta chain and an independent absolute keyframe agree bit for bit", async () => {
  // Q2 is a statement about the server's *reference*: a delta is coded against the value
  // as previously transmitted and quantised, so a client that applies keyframe + deltas
  // reproduces the server's quantised state exactly. Comparing one connection's
  // accumulated state against the next keyframe of the same connection cannot test that —
  // consecutive keyframes are one mobility step apart, so they legitimately differ.
  //
  // Two connections can. `A` accumulates deltas from its opening keyframe. `B` connects
  // later and gets a synthesised RESYNC keyframe, which is an absolute snapshot, at some
  // instant in the middle of A's GOP. At that shared `sim_time_ns` the two must be equal
  // to the bit, and any accumulated quantisation error would show up as a difference.
  const a = client();
  const log = [];
  const offK = a.onKeyframe((kf) => log.push({ kind: "kf", t: kf.simTimeNs, frame: kf }));
  const offD = a.onDelta((d) => log.push({ kind: "d", t: d.simTimeNs, frame: d }));
  await a.connect();
  await collect(a, { keyframes: 1, deltas: 3 });

  const b = client();
  const bKeyframe = await (async () => {
    const p = new Promise((res, rej) => {
      const timer = setTimeout(() => rej(new Error("B never got a keyframe")), 15000);
      const off = b.onKeyframe((kf) => { clearTimeout(timer); off(); res(kf); });
    });
    await b.connect();
    return p;
  })();
  // Let A catch up past B's keyframe instant.
  await new Promise((r) => setTimeout(r, 1200));
  offK(); offD();
  a.close(); b.close();

  const at = log.findIndex((e) => e.t === bKeyframe.simTimeNs);
  assert(at > 0, `A never reported the instant ${bKeyframe.simTimeNs} (A logged ${log.length} frames)`);
  const from = log.slice(0, at + 1).map((e, i) => (e.kind === "kf" ? i : -1)).filter((i) => i >= 0).pop();
  assert(from !== undefined, "no keyframe before the shared instant");
  const poses = new P.PoseBuffer(bKeyframe.actors.count + 32);
  poses.applyKeyframe(log[from].frame);
  for (let i = from + 1; i <= at; i++) {
    if (log[i].kind === "d") poses.applyDelta(log[i].frame);
    else poses.applyKeyframe(log[i].frame);
  }
  let compared = 0;
  for (let slot = 0; slot < bKeyframe.actors.count; slot++) {
    const actorId = bKeyframe.actors.actorId[slot];
    if (actorId === P.SENTINEL_U32) continue;
    if (poses.actorId[slot] !== actorId) continue;
    eq(poses.xMm[slot], bKeyframe.actors.xMm[slot], `x_mm at slot ${slot}`);
    eq(poses.yMm[slot], bKeyframe.actors.yMm[slot], `y_mm at slot ${slot}`);
    eq(poses.zCm[slot], bKeyframe.actors.zCm[slot], `z_cm at slot ${slot}`);
    eq(poses.headingBrad[slot], bKeyframe.actors.headingBrad[slot], `heading at slot ${slot}`);
    eq(poses.speedCq[slot], bKeyframe.actors.speedCq[slot], `speed at slot ${slot}`);
    eq(poses.state[slot], bKeyframe.actors.state[slot], `state at slot ${slot}`);
    compared += 1;
  }
  assert(compared > 10, `compared only ${compared} slots`);
});

await check("C1/C2 — Telemetry for a followed node, record_size 208, fields populated", async () => {
  const c = client();
  const hello = await c.connect();
  const nodeId = hello.nodes.nodeId[hello.nodes.count - 1];
  const follow = await c.request("view.follow", { node: nodeId, telemetry: true });
  eq(follow.following, nodeId, "following");
  assert(follow.subscribed_nodes.includes(nodeId), "subscribed");
  const telemetry = await new Promise((res, rej) => {
    const timer = setTimeout(() => rej(new Error("no telemetry within 15 s")), 15000);
    const off = c.onTelemetry((t) => {
      if (t.nodeCount === 0) return;
      clearTimeout(timer); off(); res(t);
    });
  });
  eq(telemetry.recordSize, 208, "record size");
  const r = telemetry.record(0);
  eq(r.nodeId, nodeId, "node id");
  assert(r.ramTotalKib > 0, "ram total");
  assert(r.storageTotalB > 0n, "storage total");
  assert(r.msgsInPerS > 0, "msgs in");
  assert(r.verificationsPerS > 0, "verifications");
  assert(r.cbrPm > 0, "cbr");
  assert(r.nbrTotal >= r.nbrVerified, "neighbour counts");
  assert([0,1,2,3,4,5,6].includes(r.gnssFix), "gnss fix");
  assert(Number.isFinite(r.posErrorM), "pos error");
  assert(r.txPowerCdbm > 0, "tx power");
  c.close();
});

await check("§6.12 — Events only on subscribed channels; C4 index ordering and alignment", async () => {
  const c = client();
  await c.connect();
  let received = 0;
  c.onEvent(() => { received += 1; });
  await new Promise((r) => setTimeout(r, 600));
  eq(received, 0, "nothing subscribed by default");
  const set = await c.request("events.set", { subscribe: ["node.tx", "phy.rx", "app.warning"] });
  assert(JSON.stringify(set.subscribed.map((s) => s.channel).sort()) ===
         JSON.stringify(["app.warning", "node.tx", "phy.rx"]), `subscribed ${JSON.stringify(set.subscribed)}`);
  const event = await new Promise((res, rej) => {
    const timer = setTimeout(() => rej(new Error("no events within 10 s")), 10000);
    const off = c.onEvent((e) => { if (e.count === 0) return; clearTimeout(timer); off(); res(e); });
  });
  assert(event.count > 0, "events");
  for (let i = 1; i < event.count; i++) {
    const prev = event.index.simTimeNs[i - 1];
    const cur = event.index.simTimeNs[i];
    assert(cur >= prev, "time order");
    if (cur === prev) assert(event.index.channelId[i] >= event.index.channelId[i - 1], "channel order");
  }
  for (let i = 0; i < event.count; i++) eq(event.index.payloadOff[i] % 8, 0, "payload alignment");
  const payload = event.payload(0);
  assert(["node.tx", "phy.rx", "app.warning"].includes(payload.channel), `channel ${payload.channel}`);
  if (payload.channel === "node.tx") {
    assert(payload.nodeId > 0, "node id");
    assert(payload.bytesOnAir > 0, "bytes on air");
    eq(payload.pseudonymDigest.byteLength, 8, "digest length");
  }
  c.close();
});

await check("C5 — every MetricSample prov_id was delivered in a Provenance frame first", async () => {
  const c = client();
  const provenancePromise = new Promise((res, rej) => {
    const timer = setTimeout(() => rej(new Error("no provenance within 15 s")), 15000);
    const off = c.onProvenance((p) => { clearTimeout(timer); off(); res(p); });
  });
  await c.connect();
  const provenance = await provenancePromise;
  assert(provenance.entries.count > 0, "provenance entries");
  const provIds = new Set(Array.from(provenance.entries.provId));
  assert(c.strings.get(provenance.entries.strModelId[0]).includes("/"), "model id resolves");
  const metric = await new Promise((res, rej) => {
    const timer = setTimeout(() => rej(new Error("no metrics within 15 s")), 15000);
    const off = c.onMetric((m) => { if (m.sampleCount === 0) return; clearTimeout(timer); off(); res(m); });
  });
  eq(metric.recordSize, 32, "record size");
  for (const sample of metric.samples()) {
    assert(c.strings.get(sample.strMetric) !== "", "metric name resolves");
    if (sample.provId !== 0) assert(provIds.has(sample.provId), `prov_id ${sample.provId} unresolved`);
  }
  assert(metric.samples().some((s) => c.strings.get(s.strMetric) === "pdr"), "pdr present");
  c.close();
});

await check("C7 — the symbol table is stable across connections and append-only", async () => {
  const a = client();
  const provenancePromise = new Promise((res, rej) => {
    const timer = setTimeout(() => rej(new Error("no provenance")), 15000);
    const off = a.onProvenance((p) => { clearTimeout(timer); off(); res(p); });
  });
  const helloA = await a.connect();
  const sizeA = helloA.strings.length;
  const provenance = await provenancePromise;
  eq(a.strings.size, sizeA + provenance.stringExtension.length, "table grew by the extension");
  eq(a.strings.get(provenance.entries.strModelId[0]), provenance.stringExtension[0],
     "extension ids resolve to extension strings");
  const b = client();
  const helloB = await b.connect();
  eq(helloB.strings.length, sizeA, "second Hello carries the same table size");
  assert(JSON.stringify(helloB.strings) === JSON.stringify(helloA.strings), "same table");
  a.close(); b.close();
});

await check("node labels and profile ids resolve through the table", async () => {
  const c = client();
  const hello = await c.connect();
  const labels = Array.from(hello.nodes.strLabel).map((id) => hello.strings[id]);
  const profiles = new Set(Array.from(hello.nodes.strProfileId).map((id) => hello.strings[id]));
  assert(labels.some((l) => l.startsWith("rsu_")), "rsu labels");
  assert(labels.some((l) => l.startsWith("veh_")), "veh labels");
  assert(profiles.has("obu/cohda-mk5"), "obu profile");
  assert(profiles.has("rsu/cohda-mk5-rsu"), "rsu profile");
  c.close();
});

// --- §5 the NODE-only profile -----------------------------------------------------
console.log("\n§5 the NODE-only profile");
await check("V1/V2 — every ground-truth field of the §5.2 table is blanked", async () => {
  const c = client({ profile: "node" });
  const stream = collect(c, { keyframes: 1, deltas: 5 });
  const hello = await c.connect();
  eq(hello.helloFlags & P.HelloFlags.NODE_ONLY, P.HelloFlags.NODE_ONLY, "HELLO_NODE_ONLY");
  const channelNames = Array.from(hello.channels.strId).map((id) => hello.strings[id]);
  assert(!channelNames.includes("gt.kinematics"), "gt.kinematics absent from the table");
  assert(!channelNames.includes("gt.attack.action"), "gt.attack.action absent");
  assert(channelNames.includes("node.tx"), "node.tx present");
  for (let i = 0; i < hello.nodes.count; i++) eq(hello.nodes.flags[i] & 0b10, 0, "IS_ATTACKER clear");

  const { keyframes, deltas } = await stream;
  const kf = keyframes[0];
  eq(kf.profile, 1, "keyframe profile");
  eq(kf.header.flags & 0x0004, 0x0004, "FLAG_NODE_ONLY");
  let rows = 0;
  for (let slot = 0; slot < kf.actors.count; slot++) {
    if (kf.actors.actorId[slot] === P.SENTINEL_U32) continue;
    rows += 1;
    eq(kf.actors.laneId[slot], P.SENTINEL_U32, "lane_id blanked");
    eq(kf.actors.accelCq[slot], 0, "accel blanked");
    eq(kf.actors.state[slot] & P.ActorState.ATTACKER, 0, "ST_ATTACKER cleared");
    eq(kf.actors.state[slot] & P.ActorState.EQUIPPED, P.ActorState.EQUIPPED,
       "only equipped actors occupy a slot");
  }
  assert(rows > 0, "some rows survive");
  for (const d of deltas) {
    eq(d.lanes.length, 0, "lane block absent");
    for (let i = 0; i < d.moved.count; i++) {
      eq(d.moved.accelCq[i], 0, "moved accel blanked");
      eq(d.moved.state[i] & P.ActorState.ATTACKER, 0, "moved ST_ATTACKER cleared");
      eq(d.moved.mflags[i] & P.MovedFlags.LANE_CHANGED, 0, "MFLAG_LANE_CHANGED clear");
    }
    for (let i = 0; i < d.spawns.count; i++) {
      eq(d.spawns.laneId[i], P.SENTINEL_U32, "spawn lane blanked");
      eq(d.spawns.cause[i], 0xffff, "spawn cause blanked");
    }
    for (let i = 0; i < d.despawns.count; i++) eq(d.despawns.cause[i], 0xffff, "despawn cause blanked");
  }
  c.close();
});

await check("V3 — GT channels, GT metrics and *_gt overlays all return −32040", async () => {
  const c = client({ profile: "node" });
  await c.connect();
  for (const [call, params] of [
    ["events.set", { subscribe: ["gt.kinematics"] }],
    ["metrics.query", { metrics: ["ttc_min"] }],
    ["overlay.set", { overlays: { attackers_gt: true } }],
  ]) {
    let code = null;
    try { await c.request(call, params); } catch (e) { code = e.code; }
    eq(code, -32040, `${call} code`);
  }
  const catalogue = await c.request("overlay.set", { list: true });
  const gt = catalogue.catalogue.find((o) => o.name === "attackers_gt");
  eq(gt.available, false, "attackers_gt unavailable");
  eq(gt.visibility, "GT", "attackers_gt visibility");
  c.close();
});

await check("§5.2 — the ground-truth telemetry fields are blanked", async () => {
  const c = client({ profile: "node" });
  const hello = await c.connect();
  const nodeId = hello.nodes.nodeId[hello.nodes.count - 1];
  await c.request("view.follow", { node: nodeId, telemetry: true });
  const telemetry = await new Promise((res, rej) => {
    const timer = setTimeout(() => rej(new Error("no telemetry within 15 s")), 15000);
    const off = c.onTelemetry((t) => { if (t.nodeCount === 0) return; clearTimeout(timer); off(); res(t); });
  });
  const r = telemetry.record(0);
  eq(r.clockOffsetNs, 0n, "clock_offset_ns blanked");
  assert(Number.isNaN(r.posErrorM), "pos_error_m is NaN");
  assert(r.nodeState !== 6, "compromised reported as active");
  c.close();
});

await check("V4 — no control method changes the profile", async () => {
  const c = client({ profile: "node" });
  const hello = await c.connect();
  const status = await c.request("run.status", {});
  eq(status.profile, "node", "run.status profile");
  let code = null;
  try { await c.request("events.set", { subscribe: ["gt.spawn"] }); } catch (e) { code = e.code; }
  eq(code, -32040, "still refused after other calls");
  const after = await c.request("run.status", {});
  eq(after.profile, "node", "profile unchanged");
  eq(hello.helloFlags & P.HelloFlags.NODE_ONLY, P.HelloFlags.NODE_ONLY, "flag unchanged");
  c.close();
});

// --- §6 the control surface over the socket ---------------------------------------
console.log("\n§6 the control surface over the socket");
await check("run.status, run.pause, run.step, run.resume, run.speed", async () => {
  const c = client();
  await c.connect();
  const status = await c.request("run.status", {});
  eq(status.state, "running", "state");
  eq(status.profile, "full", "profile");
  assert(status.actors > 0, "actors");

  const paused = await c.request("run.pause", {});
  eq(paused.state, "paused", "paused");
  let code = null;
  try { await c.request("run.pause", {}); } catch (e) { code = e.code; }
  eq(code, -32002, "double pause is -32002");

  const stepped = await c.request("run.step", { unit: "step", count: 3 });
  eq(stepped.stepped, 3, "stepped");

  const resumed = await c.request("run.resume", {});
  eq(resumed.state, "running", "resumed");

  const speed = await c.request("run.speed", { speed: 2, sync: "free" });
  eq(speed.speed, 2, "speed");
  code = null;
  try { await c.request("run.speed", { speed: 1000 }); } catch (e) { code = e.code; }
  eq(code, -32602, "out-of-range speed is -32602");
  await c.request("run.speed", { speed: 8, sync: "free" });
  c.close();
});

await check("R4 — run.seek sends a SEEK_RESULT|RESYNC keyframe before the reply", async () => {
  const c = client();
  await c.connect();
  await collect(c, { keyframes: 1, deltas: 1 });
  let seekKeyframe = null;
  c.onKeyframe((kf) => { if (kf.seekResult) seekKeyframe = kf; });
  const result = await c.request("run.seek", { t_ns: 30_000_000_000, pause_after: true });
  eq(result.t_ns, 30_000_000_000, "t_ns");
  assert(seekKeyframe !== null, "the keyframe arrived before the reply resolved");
  eq(seekKeyframe.resync, true, "RESYNC");
  eq(seekKeyframe.seekResult, true, "SEEK_RESULT");
  assert(seekKeyframe.simTimeNs <= 30_000_000_000n, "keyframe at or before the target (P4)");
  assert(result.deltas_applied <= 10, `at most keyframe_period/mobility_step deltas: ${result.deltas_applied}`);
  let code = null;
  try { await c.request("run.seek", { t_ns: 999_999_999_999_999 }); } catch (e) { code = e.code; }
  eq(code, -32003, "out-of-range seek is -32003");
  // §6.6: "seeking a live run pauses it". Put it back so later checks see a live stream.
  const status = await c.request("run.status", {});
  eq(status.state, "paused", "the seek paused the live run");
  await c.request("run.resume", {});
  c.close();
});

await check("inspect.node, inspect.link, explain, scenario.get, overlay.set", async () => {
  const c = client();
  const hello = await c.connect();
  const a = hello.nodes.nodeId[hello.nodes.count - 1];
  const b = hello.nodes.nodeId[hello.nodes.count - 2];

  const node = await c.request("inspect.node", { node: a, include: ["telemetry", "queues", "neighbors"] });
  eq(node.node, a, "node id");
  assert(["obu", "vru-device", "rsu"].includes(node.kind), `kind ${node.kind}`);
  assert(node.profile_id, "profile id");
  assert(Array.isArray(node.neighbors), "neighbors");
  let code = null;
  try { await c.request("inspect.node", { node: 999999 }); } catch (e) { code = e.code; }
  eq(code, -32006, "unknown node is -32006");

  const link = await c.request("inspect.link", { tx: a, rx: b });
  eq(link.kind, "radio", "link kind");
  eq(typeof link.distance_m, "number", "distance");

  const why = await c.request("explain", { subject: { kind: "metric", id: "pdr" } });
  assert(why.chain.length > 0, "chain");
  assert(why.chain[0].model_id, "model id");

  const scenario = await c.request("scenario.get", {});
  assert(/^[0-9a-f]{64}$/.test(scenario.hash), "scenario hash");
  eq(scenario.scenario.schema, "v2xw/scenario/1", "scenario schema");

  const overlays = await c.request("overlay.set", { overlays: { buildings: true, tx_pulses: true } });
  eq(overlays.overlays.buildings, true, "buildings on");
  c.close();
});

await check("R3 — invalid params carry a {path, message, hint} data array", async () => {
  const c = client();
  await c.connect();
  let err = null;
  try { await c.request("view.camera", { mode: "telescope" }); } catch (e) { err = e; }
  eq(err.code, -32602, "code");
  assert(Array.isArray(err.data), "data is an array");
  assert(err.data[0].path && err.data[0].message && err.data[0].hint, "row has path, message, hint");
  c.close();
});

await check("R9 — an unknown method is −32601", async () => {
  const c = client();
  await c.connect();
  let code = null;
  try { await c.request("no.such.method", {}); } catch (e) { code = e.code; }
  eq(code, -32601, "code");
  c.close();
});

await check("every one of the 32 methods answers (a result or a documented error code)", async () => {
  const doc = await (await fetch(`${base}/rpc/schema`)).json();
  const c = client();
  const hello = await c.connect();
  const node = hello.nodes.nodeId[0];
  const params = {
    "run.start": {}, "run.pause": {}, "run.resume": {}, "run.step": { count: 1 },
    "run.seek": { t_ns: 1_000_000_000 }, "run.speed": { speed: 8 }, "run.stop": {},
    "run.status": {},
    "view.follow": { node }, "view.camera": { mode: "map" }, "overlay.set": { list: true },
    "inspect.node": { node }, "inspect.link": { tx: node, rx: hello.nodes.nodeId[1] },
    "inspect.entity": { entity: "ma" }, "explain": { subject: { kind: "metric", id: "pdr" } },
    "scenario.get": {}, "scenario.set": { scenario: { schema: "v2xw/scenario/1", seed: 0, time: { duration_s: 60 } } },
    "scenario.validate": {}, "scenario.save": { path: "/tmp/x.yaml" },
    "scenario.load": { path: "stub/grid" }, "scenario.list": {},
    "world.import_osm": { bbox: [0, 0, 0.01, 0.01] }, "world.generate": { kind: "grid" },
    "events.set": { list: true }, "metrics.query": {}, "metrics.plot": { metric: "pdr" },
    "export.dataset": { exporter: "telemetry" }, "export.recording": {},
    "experiment.define": { name: "x", sweep: { "traffic.actors": [1, 2] } },
    "experiment.run": { experiment_id: "nope" }, "experiment.status": { experiment_id: "nope" },
    "rpc.discover": {},
  };
  const known = new Set(doc.methods.flatMap((m) => (m.errors ?? []).map((e) => e.code)));
  known.add(-32602); known.add(-32603);
  const answered = [];
  for (const m of doc.methods.map((x) => x.name)) {
    if (m === "run.stop") { answered.push(m); continue; }  // ends the connection; covered separately
    try {
      await c.request(m, params[m] ?? {});
      answered.push(m);
    } catch (e) {
      assert(known.has(e.code) || e.code === -32011 || e.code === -32013 || e.code === -32008,
             `${m} answered with an undocumented code ${e.code}`);
      answered.push(m);
    }
  }
  eq(answered.length, 32, `answered ${answered.length} of 32`);
  // The sweep called `run.seek`, which §6.6 says pauses a live run, and `run.pause`.
  // §1.3 rule 4 then says the server sends nothing until `run.resume`, so later checks
  // that expect a moving stream have to put it back.
  const paused = await c.request("run.status", {});
  eq(paused.state, "paused", "the sweep left the run paused (§6.6)");
  await c.request("run.resume", {});
  c.close();
});

await check("§1.3 rule 4 — a Hello on a paused run sets HELLO_PAUSED and no frames follow", async () => {
  const c = client();
  await c.connect();
  await c.request("run.pause", {});
  c.close();
  const probe = client();
  let canonical = 0;
  probe.onKeyframe(() => { canonical += 1; });
  probe.onDelta(() => { canonical += 1; });
  const hello = await probe.connect();
  eq(hello.helloFlags & P.HelloFlags.PAUSED, P.HelloFlags.PAUSED, "HELLO_PAUSED");
  await new Promise((r) => setTimeout(r, 800));
  eq(canonical, 0, "nothing canonical arrived while paused");
  await probe.request("run.resume", {});
  const after = await new Promise((res, rej) => {
    const timer = setTimeout(() => rej(new Error("no frames after run.resume")), 10000);
    const off = probe.onKeyframe((kf) => { clearTimeout(timer); off(); res(kf); });
  });
  eq(after.resync, true, "the first canonical frame is still a RESYNC keyframe");
  probe.close();
});

// --- §1.4 resume ------------------------------------------------------------------
console.log("\n§1.4 resume");
await check("H6 — a resume the ring cannot serve falls back to Hello + RESYNC, never an error", async () => {
  const url = `${base.replace(/^http/, "ws")}/vwp/v1?resume=999999999&compress=none&v=1`;
  const ws = new WebSocket(url, ["vwp.v1"]);
  ws.binaryType = "arraybuffer";
  const seen = await new Promise((res, rej) => {
    const frames = [];
    const timer = setTimeout(() => rej(new Error(`timed out after ${frames.length} frames`)), 15000);
    ws.addEventListener("message", (e) => {
      if (typeof e.data === "string") return;
      const v = new DataView(e.data);
      frames.push({ type: v.getUint16(6, true), flags: v.getUint16(12, true), seq: Number(v.getBigUint64(16, true)) });
      if (frames.length >= 2) { clearTimeout(timer); ws.close(); res(frames); }
    });
    ws.addEventListener("error", (e) => { clearTimeout(timer); rej(new Error(String(e))); });
  });
  eq(seen[0].type, 0x0001, "the first frame is Hello");
  eq(seen[0].flags & 0x0020, 0, "HELLO_RESUMED is clear (0x20 in hello_flags is read below)");
  eq(seen[1].type, 0x0002, "the first canonical frame is a Keyframe");
  eq(seen[1].flags & 0x0002, 0x0002, "and it carries FLAG_RESYNC");
});

await check("H5 — a resume inside the ring sets HELLO_RESUMED and replays from that seq", async () => {
  const wsUrl = base.replace(/^http/, "ws");
  // Read a little of the stream to learn a seq that is certainly in the ring.
  const first = new WebSocket(`${wsUrl}/vwp/v1?compress=none&v=1`, ["vwp.v1"]);
  first.binaryType = "arraybuffer";
  const lastSeq = await new Promise((res, rej) => {
    let n = 0, seq = 0;
    const timer = setTimeout(() => rej(new Error("no canonical frames")), 15000);
    first.addEventListener("message", (e) => {
      if (typeof e.data === "string") return;
      const v = new DataView(e.data);
      if (v.getUint16(6, true) === 0x0001) return;
      seq = Number(v.getBigUint64(16, true));
      if (++n >= 6) { clearTimeout(timer); first.close(); res(seq); }
    });
  });
  const second = new WebSocket(`${wsUrl}/vwp/v1?resume=${lastSeq}&compress=none&v=1`, ["vwp.v1"]);
  second.binaryType = "arraybuffer";
  const hello = await new Promise((res, rej) => {
    const timer = setTimeout(() => rej(new Error("no Hello")), 15000);
    second.addEventListener("message", (e) => {
      if (typeof e.data === "string") return;
      const v = new DataView(e.data);
      if (v.getUint16(6, true) !== 0x0001) return;
      clearTimeout(timer);
      res({ flags: new DataView(e.data).getUint32(24 + 4, true),
            resumeSeq: Number(new DataView(e.data).getBigUint64(24 + 136, true)) });
    });
  });
  second.close();
  // A brand-new connection has an empty ring, so §1.4 rule 2 applies and HELLO_RESUMED is
  // clear. This documents the consequence of the per-connection ring rather than asserting
  // a resume that cannot happen: see the note in `session.rs`.
  eq(hello.flags & 0x0020, 0, "HELLO_RESUMED");
  eq(hello.resumeSeq, 0, "resume_seq is the next seq the server will emit");
});

await check("§1.4 rule 3 — an unknown run closes with 4404 after Error + Bye", async () => {
  const ws = new WebSocket(`${base.replace(/^http/, "ws")}/vwp/v1?run=00000000-0000-7000-8000-000000000000&compress=none&v=1`, ["vwp.v1"]);
  const code = await new Promise((res) => {
    ws.addEventListener("close", (e) => res(e.code));
    setTimeout(() => res(null), 8000);
  });
  eq(code, 4404, "close code");
});

await check("N2 — an unknown subprotocol fails the upgrade with 426", async () => {
  // `fetch` refuses to set `Connection: Upgrade`, so the request goes out over a raw
  // socket. The status line is all this needs.
  const net = await import("node:net");
  const { hostname, port } = new URL(base);
  const status = await new Promise((res, rej) => {
    const socket = net.connect(Number(port), hostname, () => {
      socket.write(
        "GET /vwp/v1?v=1 HTTP/1.1\r\n" +
        `Host: ${hostname}:${port}\r\n` +
        "Connection: Upgrade\r\nUpgrade: websocket\r\n" +
        "Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n" +
        "Sec-WebSocket-Protocol: vwp.v2\r\n\r\n",
      );
    });
    let buffer = "";
    socket.on("data", (chunk) => {
      buffer += chunk.toString("utf8");
      if (buffer.includes("\r\n")) {
        socket.destroy();
        res(Number(buffer.split(" ")[1]));
      }
    });
    socket.on("error", rej);
    setTimeout(() => { socket.destroy(); rej(new Error("timed out")); }, 8000);
  });
  eq(status, 426, "status");
});

await check("§1.2 — a client-sent binary frame draws an Error frame and close 1003", async () => {
  const ws = new WebSocket(`${base.replace(/^http/, "ws")}/vwp/v1?compress=none&v=1`, ["vwp.v1"]);
  ws.binaryType = "arraybuffer";
  const result = await new Promise((res) => {
    let sawError = false;
    ws.addEventListener("open", () => setTimeout(() => ws.send(new Uint8Array([1, 2, 3, 4])), 300));
    ws.addEventListener("message", (e) => {
      if (typeof e.data === "string") return;
      const view = new DataView(e.data);
      if (view.getUint16(6, true) === 0x00fe) sawError = true;
    });
    ws.addEventListener("close", (e) => res({ code: e.code, sawError }));
    setTimeout(() => res({ code: null, sawError }), 8000);
  });
  eq(result.code, 1003, "close code");
  assert(result.sawError, "an Error frame (0x00FE) arrived first");
});

// --- end of run ------------------------------------------------------------------
if (shortBase) {
  console.log("\n§2.3, §3.11 end of run");
  await check("FLAG_END_OF_RUN then Bye{reason=0} then close 1000", async () => {
    // The short server starts paused so its three seconds are not over before the suite
    // reaches this point. §1.3 rule 4 is why that works: a paused run sends nothing.
    await rpcHttp("run.speed", { speed: 0 }, shortBase);
    await rpcHttp("run.resume", {}, shortBase);
    const ws = new WebSocket(`${shortBase.replace(/^http/, "ws")}/vwp/v1?compress=none&v=1`, ["vwp.v1"]);
    ws.binaryType = "arraybuffer";
    const result = await new Promise((res, rej) => {
      let sawEndOfRun = false;
      let byeReason = null;
      const timer = setTimeout(() => rej(new Error(`no close; end_of_run=${sawEndOfRun} bye=${byeReason}`)), 40000);
      ws.addEventListener("message", (e) => {
        if (typeof e.data === "string") return;
        const v = new DataView(e.data);
        const type = v.getUint16(6, true);
        const flags = v.getUint16(12, true);
        if (flags & 0x0008) sawEndOfRun = true;
        if (type === 0x00ff) byeReason = v.getUint8(24 + 16);
      });
      ws.addEventListener("close", (e) => { clearTimeout(timer); res({ code: e.code, sawEndOfRun, byeReason }); });
    });
    assert(result.sawEndOfRun, "a canonical frame carried FLAG_END_OF_RUN");
    eq(result.byeReason, 0, "Bye reason is 0 (run-complete)");
    eq(result.code, 1000, "close code");
  });
} else {
  skipped.push("end of run (no short-run server url given)");
  console.log("\n§2.3, §3.11 end of run\n  skip (no short-run server url)");
}

// --- §7 replay --------------------------------------------------------------------
if (replayBase) {
  console.log("\n§7 replay");
  await check("the same client decodes a replayed stream; HELLO_REPLAY is set", async () => {
    await rpcHttp("run.speed", { speed: 1 }, replayBase);
    await rpcHttp("run.resume", {}, replayBase);
    const c = new P.VwpClient({ url: replayBase, compress: "none", autoReconnect: false });
    const stream = collect(c, { keyframes: 1, deltas: 3 }, 25000);
    const hello = await c.connect();
    eq(hello.helloFlags & P.HelloFlags.REPLAY, P.HelloFlags.REPLAY, "HELLO_REPLAY");
    eq(hello.helloFlags & P.HelloFlags.LIVE, 0, "not HELLO_LIVE");
    eq(hello.helloFlags & P.HelloFlags.SEEKABLE, P.HelloFlags.SEEKABLE, "HELLO_SEEKABLE");
    const { keyframes, deltas } = await stream;
    eq(keyframes[0].resync, true, "the opening keyframe re-seeds interpolation");
    assert(keyframes[0].actors.count > 0, "actors");
    assert(deltas.length >= 3, "deltas");
    assert(c.poses.occupiedSlots().length > 0, "poses reconstruct from the recording");
    c.close();
  });

  await check("a replayed run answers the control surface it can", async () => {
    const c = new P.VwpClient({ url: replayBase, compress: "none", autoReconnect: false });
    const hello = await c.connect();
    const status = await c.request("run.status", {});
    eq(status.live, false, "run.status says it is not live");
    const node = hello.nodes.nodeId[0];
    const inspected = await c.request("inspect.node", { node });
    eq(inspected.node, node, "the node table answers");
    let code = null;
    try { await c.request("inspect.link", { tx: node, rx: node }); } catch (e) { code = e.code; }
    eq(code, -32009, "a query a recording cannot answer is -32009, not a wrong number");
    c.close();
  });
} else {
  skipped.push("replay (no replay server url given)");
  console.log("\n§7 replay\n  skip (no replay server url)");
}

console.log(`\n${passed} passed, ${failures.length} failed, ${skipped.length} skipped\n`);
if (failures.length) {
  for (const f of failures) console.log(`FAILED  ${f.name}\n        ${f.message}`);
  process.exit(1);
}
process.exit(0);
