# `@vwp/mock-server`

A VWP v1 **server** you can develop the whole UI against without the Rust engine. It serves a
synthetic Manhattan-like world, drives N actors along it, and speaks the binary stream and the
JSON-RPC control surface of `docs/protocol/vwp-v1.md`.

It is a development and test fixture, not production code — but the bytes it writes are the
specification's bytes, which is what makes it a useful check on the client decoder.

```sh
pnpm -r build
node packages/mock-server/dist/index.js --actors 500 --port 8787
```

| Option | Default | Meaning |
|---|---|---|
| `--actors <n>` | 200 | actors to drive; 5000 is the stress point the UI must handle |
| `--port <n>` | 8787 | TCP port (`0` picks a free one) |
| `--host <addr>` | 127.0.0.1 | bind address |
| `--speed <x>` | 1 | multiple of real time; `0` produces as fast as the timer allows |
| `--seed <n>` | 20260918 | world and traffic seed; generation is deterministic in it |
| `--bbox <l,b,r,t>` | midtown Manhattan | WGS-84 bbox to generate over |
| `--paused` | off | start the run paused at `t = 0` (`HELLO_PAUSED`) |

## What it serves

| Endpoint | |
|---|---|
| `GET /vwp/v1?run&resume&profile&compress&v` | the stream, subprotocol `vwp.v1` |
| `GET /world/{hash}.vwb` | the world, `vwp-world/1` binary (§4) |
| `GET /world/{hash}.json` | the world, JSON mirror (§4.6) |
| `POST /rpc` | one-shot JSON-RPC (§6.2) |
| `GET /rpc/schema` | an OpenRPC 1.3.2 document naming all 32 methods |
| `GET /healthz` | `{"ok":true,…}` |

Every HTTP response carries the COOP/COEP/CORP headers §1.1 requires, and world responses are
`immutable` with an `ETag`.

**The world** is a ~1.86 × 2.0 km midtown grid: avenues 274 m apart, cross-streets 80 m apart,
rotated 29° east of north, clipped to the bbox and projected to local ENU metres — about 920 lanes,
520 extruded building footprints, 170 signalised junctions, 600 signal heads, 12 RSU sites, 340
crossings and a park. It is deterministic in `--seed`, so `world_hash` is reproducible (§10.5 W5).

**The stream** is `Hello` → RESYNC `Keyframe` → `Delta` every 100 ms of sim time, a `Keyframe`
every 1 s, `Provenance` after the opening keyframe, `MetricSample` every 1 s, `Telemetry` every 1 s
for nodes subscribed through `view.follow`, and `Event` batches on channels subscribed through
`events.set` (§6.12: nothing is subscribed by default).

**The control surface** implements `run.start`/`pause`/`resume`/`step`/`seek`/`speed`/`stop`/`status`,
`view.follow`, `view.camera`, `overlay.set`, `inspect.node`/`link`/`entity`, `explain`,
`scenario.get`/`list`/`validate`, `events.set`, `metrics.query` and `rpc.discover`. Every other
method of §6.15 answers `-32009` with `data.why`, so a caller can tell "not implemented here" from
"no such method" (`-32601`).

## What it does exactly

- **§3.2 delta references** are the previously **transmitted quantised** values, held per slot, so
  keyframe + every delta reproduces the server's state bit for bit and error never accumulates.
  The interop test asserts this against the next keyframe's own columns.
- **§3.2 escape hatch**: a step over ±32,000 mm sets `MFLAG_ABSOLUTE` and goes in the absolute block.
- **§3.3.1 slots**: lowest free slot at spawn, released one full keyframe period after despawn.
- **§5.2 blanking** happens at the producer, per connection profile, before serialisation — lane
  ids, acceleration, `ST_ATTACKER`, spawn/despawn causes, the delta lane block, `IS_ATTACKER`,
  GT channels, GT metrics, `clock_offset_ns`, `pos_error_m`, `node_state = 6`, and unequipped
  actors' slots. About 12 % of actors are unequipped so that last rule is actually exercised.
- **§1.5 backpressure**: deltas are dropped all-or-nothing when a socket is over 8 MiB queued, a
  RESYNC keyframe follows within 250 ms, and the drop is reported as a `stream.drop` notification.
- **§1.4 resume**: a 4096-frame / 8 MiB ring; `?resume=<seq>` inside it replays without a gap.

## Where it deliberately differs

These are fixture simplifications, not protocol deviations, and each is visible to a client only as
a stream that is *less* dense than a real engine's:

1. **Signal countdowns in deltas.** §3.4.7 lets a delta carry only signals whose `phase` or
   `time_to_change_ds` changed. The countdown changes every 100 ms, so a strict producer would
   re-send all 600 heads ten times a second. This server sends the whole set on a phase change and
   otherwise every fifth step, leaving a client's countdown up to 400 ms stale.
2. **Never compresses.** `FLAG_COMPRESSED` is never set, so clients must pass `compress=none`.
3. **`seq` and per-connection content.** §1.4 defines `seq` as run-global and deterministic, but
   `Event` and `Telemetry` content depends on per-connection subscriptions. This server allocates
   one `seq` per emission batch, shared by every connection, which is exact for the single-client
   case and keeps the sequence dense. See the spec note in the return value.
4. **Resume is full-profile only.** The ring stores the canonical full-profile frames; a
   `node`-profile reconnect falls back to `Hello` + RESYNC keyframe, which §1.4 case 2 always allows.
5. **No MCAP recording, no replay, no experiments, no exports.** Those methods answer `-32009`.
6. **Junction geometry.** Actors cross junctions on a server-side straight connector between lane
   ends rather than on junction-internal lanes, so motion is continuous but the world payload has
   no `lane_type = 5` lanes.
