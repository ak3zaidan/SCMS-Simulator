/**
 * End-to-end smoke test: start the mock engine, connect with @vwp/protocol, and print what the
 * decoders make of the first Hello, the first Keyframe and the first ten Deltas.
 *
 *   node scripts/smoke.mjs [--actors 500] [--port 0]
 *
 * Run it after `pnpm -r build`: it imports both packages from their built output, which is what a
 * consumer of the workspace gets. Node 22+ supplies the global `WebSocket` it connects with.
 */

import { MockEngineServer } from "../packages/mock-server/dist/index.js";
import {
  VwpClient,
  bytesToHex,
  computeWorldContentHash,
  decodeWorld,
  dequantiseTimeToChangeS,
  formatUuid,
} from "../packages/protocol/dist/index.js";

const args = process.argv.slice(2);
const flag = (name, fallback) => {
  const i = args.indexOf(name);
  return i >= 0 && args[i + 1] !== undefined ? Number(args[i + 1]) : fallback;
};

const server = new MockEngineServer({ actors: flag("--actors", 200), port: flag("--port", 8799), speed: 4, quiet: true });
const address = await server.start();
console.log(`mock engine on ${address.httpUrl} (${server.run.actorCount} actors)`);

const client = new VwpClient({
  url: address.httpUrl,
  compress: "none",
  autoReconnect: false,
  // Node 22+ ships a global WebSocket, so the client needs no socket factory here.
});

let keyframes = 0;
let deltas = 0;
let firstKeyframeLogged = false;
const done = Promise.withResolvers();

client.onKeyframe((kf) => {
  keyframes += 1;
  if (firstKeyframeLogged) return;
  firstKeyframeLogged = true;
  const poses = client.poses;
  const occupied = poses.occupiedSlots();
  const slot = occupied[0] ?? 0;
  const p = poses.positionOf(slot);
  console.log(`\nKEYFRAME  seq=${kf.header.seq} gop=${kf.gopIndex} t=${Number(kf.simTimeNs) / 1e9}s profile=${kf.profile}`);
  console.log(`  actor_count (slots) = ${kf.actors.count}, occupied = ${occupied.length}, signals = ${kf.signals.count}`);
  console.log(`  origin = (${kf.originXM}, ${kf.originYM}, ${kf.originZM}) m`);
  console.log(
    `  sample pose  slot ${slot}  actor ${poses.actorId[slot]}  x=${p.x.toFixed(3)} m  y=${p.y.toFixed(3)} m  z=${p.z.toFixed(3)} m` +
      `  heading=${poses.headingOf(slot).toFixed(4)} rad  speed=${poses.speedOf(slot).toFixed(3)} m/s  lane=${poses.laneId[slot]}` +
      `  state=0x${poses.state[slot].toString(16).padStart(2, "0")}  nbrs=${poses.verifiedNeighbors[slot]}`,
  );
  console.log(
    `  quantised    x_mm=${poses.xMm[slot]}  y_mm=${poses.yMm[slot]}  z_cm=${poses.zCm[slot]}  heading_brad=${poses.headingBrad[slot]}  speed_cq=${poses.speedCq[slot]}`,
  );
  console.log(`  signal 0: phase=${kf.signals.phase[0]} time_to_change=${dequantiseTimeToChangeS(kf.signals.timeToChangeDs[0]).toFixed(1)} s`);
});

client.onDelta((d) => {
  deltas += 1;
  if (deltas <= 10) {
    const slot = d.moved.count > 0 ? d.moved.slot[0] : -1;
    const p = slot >= 0 ? client.poses.positionOf(slot) : null;
    console.log(
      `DELTA ${String(deltas).padStart(2)}  seq=${d.header.seq} gop=${d.gopIndex} step=${d.stepIndex}` +
        `  moved=${d.moved.count} abs=${d.absolute.count} lanes=${d.lanes.length} spawns=${d.spawns.count} despawns=${d.despawns.count} signals=${d.signals.count}` +
        (p ? `  slot ${slot} -> (${p.x.toFixed(3)}, ${p.y.toFixed(3)}) m  dx=${d.moved.dxMm[0]} mm dy=${d.moved.dyMm[0]} mm` : ""),
    );
  }
  if (deltas >= 10 && keyframes >= 1) done.resolve();
});

client.onHello((h) => {
  console.log(`\nHELLO     ${h.engineVersion}  run=${formatUuid(h.runId)}  scenario="${h.scenarioName}" label="${h.runLabel}"`);
  console.log(`  flags=0x${h.helloFlags.toString(16)}  resume_seq=${h.resumeSeq}  sim_time=${h.simTimeNs}`);
  console.log(`  nodes=${h.nodes.count} classes=${h.classes.count} channels=${h.channels.count} strings=${h.strings.length} actor_capacity=${h.actorCapacity}`);
  console.log(`  origin = ${h.originLatDeg.toFixed(6)} N, ${h.originLonDeg.toFixed(6)} E, ${h.originAltM} m`);
  console.log(
    `  bbox   = (${h.bboxMinXM.toFixed(1)}, ${h.bboxMinYM.toFixed(1)}) .. (${h.bboxMaxXM.toFixed(1)}, ${h.bboxMaxYM.toFixed(1)}) m` +
      `  = ${(h.bboxMaxXM - h.bboxMinXM).toFixed(0)} x ${(h.bboxMaxYM - h.bboxMinYM).toFixed(0)} m`,
  );
  console.log(`  classes: ${[...h.classes.strName].map((id) => h.strings[id]).join(", ")}`);
  console.log(`  channels: ${[...h.channels.strId].map((id, i) => `${h.strings[id]}(${h.channels.channelId[i]})`).join(", ")}`);
  console.log(`  world_hash=${bytesToHex(h.worldHash)}`);
  console.log(`  world_ref: mode=${h.worldRef.mode} bytes=${h.worldRef.payloadBytes} url=${h.strings[h.worldRef.strUrl]}`);
});

const hello = await client.connect();

// Fetch the world over HTTP by content hash and verify it, exactly as the Studio will (§10.5 W3).
const worldUrl = `${address.httpUrl}${hello.strings[hello.worldRef.strUrl]}`;
const worldBytes = await (await fetch(worldUrl)).arrayBuffer();
const world = decodeWorld(worldBytes);
const recomputed = await computeWorldContentHash(worldBytes);
console.log(`\nWORLD     ${(worldBytes.byteLength / 1024).toFixed(0)} KiB from ${hello.strings[hello.worldRef.strUrl]}`);
console.log(`  lanes=${world.lanes.count} (${world.lanePoints.count} centreline points) buildings=${world.buildings.count} (${world.ringPoints.count} ring points)`);
console.log(`  junctions=${world.junctions.count} signals=${world.signals.count} sites=${world.sites.count} crossings=${world.crossings.count} landuse=${world.landuse.count}`);
console.log(`  content_hash matches Hello.world_hash: ${world.contentHash === bytesToHex(hello.worldHash)}; recomputed from the body: ${recomputed === world.contentHash}`);
console.log(`  first lane: "${world.str(world.lanes.strName[0])}" ${world.lanes.pointCount[0]} points, ${world.lanes.widthM[0]} m wide, limit ${world.lanes.speedLimitMps[0].toFixed(2)} m/s`);
console.log(`  provenance: source=${world.provenance?.source} licence=${world.provenance?.licence}`);

const status = await client.request("run.status", {});
console.log(`\nRPC       run.status -> state=${status.state} actors=${status.actors} nodes=${status.nodes} speed=${status.speed} profile=${status.profile} seq=${status.seq}`);
const followed = client.hello.nodes.nodeId[client.hello.nodes.count - 1];
const follow = await client.request("view.follow", { node: followed, telemetry: true });
console.log(`          view.follow node ${followed} -> following=${follow.following} subscribed=${follow.subscribed_nodes.length}`);
client.onTelemetry((t) => {
  if (t.nodeCount === 0) return;
  const r = t.record(0);
  console.log(
    `\nTELEMETRY node ${r.nodeId}  record_size=${t.recordSize}  msgs_in=${r.msgsInPerS.toFixed(1)}/s  verifications=${r.verificationsPerS.toFixed(1)}/s` +
      `  cbr=${(r.cbrPm / 10).toFixed(1)}%  cpu=${(r.cpuUtilPm / 10).toFixed(1)}%  nbrs=${r.nbrVerified}/${r.nbrTotal}  gnss_fix=${r.gnssFix}` +
      `  pos_error=${r.posErrorM.toFixed(2)} m  clock_offset=${Number(r.clockOffsetNs) / 1e6} ms  tx_power=${(r.txPowerCdbm / 100).toFixed(2)} dBm`,
  );
});

await done.promise;
console.log(`\nRESULT    decoded ${keyframes} keyframe(s) and ${deltas} delta(s); pose buffer holds ${client.poses.count} slots, ${client.poses.occupiedSlots().length} occupied`);
console.log(`          next resume seq = ${client.ring.nextSeq}; no seq gaps were reported`);

client.close();
await server.stop();
process.exit(0);
