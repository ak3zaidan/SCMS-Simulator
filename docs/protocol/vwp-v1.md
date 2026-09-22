# VWP v1 — V2X World Simulator wire protocol (engine ↔ UI)

Status: **normative, implementable**. Version `1.0`. Date: 2026-09-18.

Implements `docs/adr/0008-recording-format-and-engine-ui-protocol.md`, `docs/design/02-architecture.md` §9,
`docs/design/09-ui.md` §2/§5/§7/§8, `docs/design/03-interfaces.md` §14, `docs/design/06-node-models.md` §2.4.

This document is written so that a Rust server implementer and a TypeScript client implementer, working
independently and without talking to each other, produce interoperable code. Everything is concrete; there
is no `TBD`. Where the design documents left a choice open, the choice is made here and marked
**`DECISION`** with a one-line reason. All such lines are collected in Appendix C.

## Contents

| § | Section |
|---|---|
| 0 | [Conventions](#0-conventions) |
| 1 | [Transport](#1-transport) — endpoint, binary vs text, handshake, reconnect/resume, backpressure |
| 2 | [Binary framing](#2-binary-framing) — 24-byte header, alignment, flags, message types, symbol table, compression |
| 3 | [Message layouts](#3-message-layouts) — `Hello`, quantisation, `Keyframe`, `Delta`, `Telemetry`, `Event`, `MetricSample`, `Provenance`, `WorldChunk`, `Error`, `Bye` |
| 4 | [The world payload `vwp-world/1`](#4-the-world-payload--vwp-world1) |
| 5 | [Visibility and the `NODE-only` profile](#5-visibility-and-the-node-only-profile) |
| 6 | [JSON-RPC 2.0 control surface](#6-json-rpc-20-control-surface) — 32 methods, 8 notifications, error codes |
| 7 | [Replay](#7-replay) — MCAP mapping, byte-identity, seek algorithm, 100 ms budget |
| 8 | [Versioning](#8-versioning) |
| 9 | [Worked example](#9-worked-example-unit-test-vectors) — annotated hex dumps (§9.1–§9.4, including the §9.4 vertical-delta vector) + reference decoders (§9.5) |
| 10 | [Conformance checklist](#10-conformance-checklist) |
| A | [Enum reference](#appendix-a--enum-reference) |
| B | [Message-type summary](#appendix-b--message-type-summary) |
| C | [Decision log](#appendix-c--decision-log) |
| D | [Things this document invented](#appendix-d--things-this-document-invented) |

---

## 0. Conventions

- **MUST / MUST NOT / SHOULD / MAY** are used as in RFC 2119.
- All multi-byte integers and floats are **little-endian**. IEEE-754 binary32 (`f32`) and binary64 (`f64`).
- Byte offsets written `@n` are relative to the start of the enclosing structure unless stated otherwise.
- A "sentinel" value means "absent": `0xFFFF_FFFF` for `u32` ids, `0xFFFF` for `u16`, `0xFF` for `u8`,
  `NaN` for floats, `u64::MAX` for `u64` times.
- `SimTime` is `u64` nanoseconds since `t0` (03-interfaces §1). Wall-clock times are `i64` nanoseconds
  since the Unix epoch, UTC.
- Spatial coordinates are **world-local ENU metres** (East, North, Up) about `Hello.origin_{lat,lon,alt}`,
  matching `Vec3` in 03-interfaces §1.
- Headings follow 03-interfaces §1: `0` = East, counter-clockwise, in the ENU plane.
- "Reserved" bytes MUST be written as zero by the sender and MUST be ignored by the reader.

### 0.1 Naming

| Term | Meaning |
|---|---|
| **frame** | one WebSocket message (binary or text) |
| **canonical frame** | a binary frame that is part of the recorded stream: `Keyframe`, `Delta`, `Telemetry`, `Event`, `MetricSample`, `Provenance`, `WorldChunk` |
| **connection frame** | `Hello`, `Error`, `Bye` — connection-scoped, not part of the canonical stream |
| **GOP** | group of pictures: one `Keyframe` plus the `Delta`s that follow it until the next `Keyframe` |
| **slot** | a stable `u32` index identifying an actor's row for its lifetime (09-ui §2 "actor slot") |
| **profile** | `full` or `node`; see §5 |

---

## 1. Transport

### 1.1 Endpoint

One WebSocket endpoint per engine process:

```
GET /vwp/v1?run=<run_id>&resume=<seq>&profile=<full|node>&compress=<zstd|none>&v=1
Upgrade: websocket
Sec-WebSocket-Protocol: vwp.v1
```

The server MUST echo `Sec-WebSocket-Protocol: vwp.v1`. A server that does not implement `vwp.v1` MUST fail
the upgrade with HTTP 426. **`DECISION`: the subprotocol token carries the major version so a mismatched
client fails at the handshake instead of at the first frame.**

Query parameters (all optional):

| Param | Default | Meaning |
|---|---|---|
| `run` | the server's current run | run id (36-char UUID string) to attach to, or `latest` |
| `resume` | absent | canonical `seq` to resume from (§1.4) |
| `profile` | `full` | `full` or `node` (§5). Immutable for the life of the connection. |
| `compress` | `zstd` | `zstd` or `none`. A client that cannot decompress zstd MUST pass `compress=none`. |
| `v` | `1` | protocol major version the client speaks |

Companion HTTP endpoints on the same origin and port:

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/world/{hash}.vwb` | world geometry, `vwp-world/1` binary (§4) |
| `GET` | `/world/{hash}.json` | world geometry, `vwp-world/1` JSON (§4.6) |
| `POST` | `/rpc` | one-shot JSON-RPC (§6.2) for CLI/notebook/CI clients |
| `GET` | `/rpc/schema` | the OpenRPC 1.3.2 document (`rpc.discover` result, §6.3) |
| `GET` | `/healthz` | `200 {"ok":true,"engine":"...","runs":[...]}` |

All HTTP responses and the WebSocket upgrade MUST carry, per 09-ui §2 (cross-origin isolation for
`SharedArrayBuffer`):

```
Cross-Origin-Opener-Policy: same-origin
Cross-Origin-Embedder-Policy: require-corp
Cross-Origin-Resource-Policy: same-origin
```

World responses are content-addressed and therefore immutable:
`Cache-Control: public, max-age=31536000, immutable` and `ETag: "<hash>"`.

Binding and auth follow 02-architecture §12: localhost by default; a non-loopback bind requires
`Authorization: Bearer <token>` on the upgrade and on every HTTP request, and the server MUST reject
missing/incorrect tokens with HTTP 401 (upgrade) — never with a WebSocket close, so the browser sees a
real status.

### 1.2 Telling binary from text

WebSocket already separates the two at the framing layer; VWP adds no in-band discriminator.

- **Binary frames** (opcode `0x2`) are VWP telemetry frames (§2). Nothing else is ever sent as binary.
- **Text frames** (opcode `0x1`) are UTF-8 JSON-RPC 2.0 messages (§6). Nothing else is ever sent as text.

TypeScript client:

```ts
ws.binaryType = "arraybuffer";
ws.onmessage = (ev) => {
  if (typeof ev.data === "string") handleJsonRpc(JSON.parse(ev.data));
  else handleVwpFrame(new DataView(ev.data as ArrayBuffer));
};
```

Rust server (`tokio-tungstenite`): `Message::Binary(_)` vs `Message::Text(_)`.

A conforming client MUST NOT send binary frames in v1; a server receiving one MUST reply with an `Error`
frame (`code = -32600`) and MAY close with 1003. A server MUST NOT send a text frame that is not valid
JSON-RPC 2.0.

`Ping`/`Pong` (opcodes `0x9`/`0xA`) are used for liveness: the server MUST send a Ping every 15 s of wall
time and MUST close the connection with 1001 if no Pong arrives within 30 s.

### 1.3 Handshake

**The client sends nothing first.** Immediately after the WebSocket handshake completes, the server sends
exactly one `Hello` binary frame (§3.1), within 1000 ms wall time. Sequence:

```
client                                    server
  |---- HTTP GET /vwp/v1 (Upgrade) ------->|
  |<--- 101 Switching Protocols -----------|
  |<=== BINARY Hello ======================|   <= mandatory, first frame, always uncompressed
  |          (client may now fetch the world over HTTP by Hello.world_hash)
  |---- TEXT {"jsonrpc":"2.0",...} ------->|   <= control, at any time
  |<=== BINARY Keyframe (FLAG_RESYNC) =====|
  |<=== BINARY Delta ======================|
  |<=== BINARY Telemetry / Event / Metric =|
```

Rules:

1. `Hello` MUST be the first frame and MUST NOT be compressed (`FLAG_COMPRESSED` clear), so a client can
   parse it before it knows whether it can decompress.
2. The first canonical frame after a non-resumed `Hello` MUST be a `Keyframe` with `FLAG_RESYNC` set.
3. A client MUST NOT assume it has the world before it has fetched it; it MUST buffer or discard
   `Keyframe`/`Delta` frames until the world is loaded (it may also render actors on a blank ground plane).
4. If the run is not started (`hello_flags & HELLO_PAUSED`), the server sends the `Hello` and then nothing
   until `run.start`/`run.resume`.
5. A server that cannot serve the requested major version MUST send `Error{code:-32050}` followed by `Bye`
   and close with code **4406**.

### 1.4 Reconnect and resume

Every canonical frame carries a 64-bit `seq` (§2.1) assigned by the *producer* (live engine or replay
reader) in canonical emission order, starting at 0 for the first canonical frame of the run. `seq` is
deterministic: the same run replayed produces the same `seq` for the same frame.

The server keeps a per-run **resume ring** of recently produced canonical frames. **`DECISION`: the resume
ring holds `max(2 GOPs, 8 MiB)` and at most 4096 frames — two GOPs guarantee that any resume point is
preceded by a retained keyframe, and 8 MiB bounds memory at ~10,000 actors.**

On reconnect the client sends `?run=<run_id>&resume=<seq>` where `seq` is one past the last frame it fully
applied. The server then:

1. **Resumable** — `seq` is in the ring *and* the `Keyframe` opening `seq`'s GOP is also in the ring:
   the server sends `Hello` with `HELLO_RESUMED` set, `resume_seq = seq`, and then replays the ring from
   `seq`. The client keeps its world, string table, actor slots and camera state.
2. **Not resumable** — the server sends `Hello` *without* `HELLO_RESUMED` (`resume_seq` = the next seq it
   will emit), then a `Keyframe` with `FLAG_RESYNC`. The client MUST discard all stream state except a
   world whose content hash equals `Hello.world_hash`, and MUST reset its string table (§2.5) to empty.
3. **Unknown run** — `Error{code:-32000}` then `Bye{reason=3}`, close 4404.

A new connection for a run that already has a connection with the same session token supersedes the old
one: the server sends `Bye{reason = 4 (superseded)}` on the old socket and closes it with 1012.

The client SHOULD reconnect with exponential backoff 250 ms → 8 s with ±20 % jitter, and MUST stop
retrying after `Bye{reason = 0 (run-complete)}` or close code 4406/4404.

### 1.5 Backpressure — the server never queues unboundedly

The simulation loop MUST NOT block on the socket. Each connection has a bounded send queue:

| Parameter | Default | Meaning |
|---|---|---|
| `max_queued_frames` | 64 | frames awaiting the socket |
| `max_queued_bytes` | 8 MiB | sum of queued frame sizes |
| `stall_timeout_s` | 30 | wall seconds over cap before the server gives up |
| `resync_deadline_ms` | 250 | wall ms after a drop within which a Keyframe must be emitted |

Frames have a fixed priority class:

| Class | Frames | Policy |
|---|---|---|
| P0 | `Hello`, `Error`, `Bye`, `WorldChunk`, all JSON-RPC text | **never dropped**; if P0 cannot be queued the connection is closed with 1011 |
| P1 | `Keyframe` | **never dropped**, but **coalesced**: at most one queued keyframe; a newer one replaces an older one |
| P2 | `Delta` | droppable, all-or-nothing (see below) |
| P3 | `Telemetry`, `MetricSample`, `Event` | droppable, oldest-first, per class |

On enqueue, if either cap would be exceeded the server MUST, in this order, until it is under cap:

1. Drop **every** queued `Delta` and set `resync_pending = true`. Deltas are never partially dropped — a
   surviving delta after a dropped one would be applied against the wrong base (§3.4).
2. Drop queued P3 frames oldest-first, counting them per class.
3. Replace the queued `Keyframe` with the newest one.
4. If still over cap, stop producing frames for this connection and record the time; if that condition
   persists for `stall_timeout_s`, send `Bye{reason=3}` and close with **1013 (Try Again Later)**.

When `resync_pending` is true the server MUST emit a `Keyframe` with `FLAG_RESYNC` at the next keyframe
boundary, or — if that boundary is more than `resync_deadline_ms` of wall time away — **synthesise one
immediately**. Synthesising is always legal: a keyframe is an idempotent snapshot of the current state.
An out-of-band keyframe still consumes a `seq` and still carries the canonical body, so a client that
records the stream gets a valid (if denser) GOP structure.

Drops are reported to the client as a **JSON-RPC notification**, not in a binary frame, so that binary
frames remain byte-identical between live and replay (§7.2):

```json
{"jsonrpc":"2.0","method":"stream.drop",
 "params":{"seq_first":10422,"seq_last":10461,
           "dropped":{"delta":38,"event":2,"telemetry":0,"metric":0},
           "resync_seq":10462}}
```

The client MUST treat a `seq` gap as a drop even if the notification is lost, and MUST NOT interpolate
across a `FLAG_RESYNC` keyframe (it re-seeds its pose rings from it instead).

**Live pacing.** By default a slow client does not slow the engine. `run.speed` with `"sync":"client"`
(§6.6) makes the engine block on a high-water mark so demos stay lossless; this is the only mode in which
the socket can throttle the simulation, and the manifest records it.

---

## 2. Binary framing

### 2.1 Frame header — 24 bytes, fixed

Every binary frame is `header (24 B) || body`.

| @ | Size | Type | Field | Value / meaning |
|---|---|---|---|---|
| 0 | 4 | `u32` | `magic` | `0x31505756`; its little-endian wire bytes are `56 57 50 31` = ASCII `V W P 1` |
| 4 | 2 | `u16` | `version` | protocol **major** version. `1` in this document. |
| 6 | 2 | `u16` | `msg_type` | §2.4 |
| 8 | 4 | `u32` | `body_len` | length in bytes of the **uncompressed** body |
| 12 | 2 | `u16` | `flags` | §2.3 |
| 14 | 2 | `u16` | `reserved` | `0` |
| 16 | 8 | `u64` | `seq` | canonical sequence number (§1.4) |

Body starts at frame offset **24**.

If `FLAG_COMPRESSED` is clear, the remaining bytes of the WebSocket message are the body and their length
MUST equal `body_len`. If set, the remaining bytes are a zstd frame (RFC 8878) whose decompressed length
MUST equal `body_len`.

A reader MUST reject a frame whose `magic` is wrong (close 1002), MUST ignore a frame whose `msg_type` it
does not know, and MUST ignore trailing bytes beyond the layouts defined here (§8).

### 2.2 Alignment

**`DECISION`: every scalar array in a body is placed at a body-relative offset that is a multiple of its
element size, so both Rust (`bytemuck::cast_slice`) and TypeScript (`new Float64Array(buf, off, n)`) can
build views with zero copying and zero parsing.**

Because the header is 24 bytes and 24 is a multiple of 8, an offset that is 8-, 4- or 2-aligned relative
to the body is equally aligned relative to the frame. A client that decompresses a body into a fresh
`ArrayBuffer` gets body offset 0, which satisfies the same rule. Therefore:

- A TypeScript client MAY view an uncompressed frame in place: `new Int32Array(frame, 24 + off, n)`.
- WebSocket `ArrayBuffer`s delivered by browsers start at byte 0 of their own buffer, so this is safe.
- Sections are padded with zero bytes to the next multiple of 4 unless a stricter alignment is stated.
- Every `off_*` field is a `u32` body-relative byte offset; **`0` means "section absent"** (a section can
  never legitimately start at 0, because every body begins with a fixed prefix).

### 2.3 Flags

| Bit | Mask | Name | Class | Meaning |
|---|---|---|---|---|
| 0 | `0x0001` | `FLAG_COMPRESSED` | transport | body is a zstd frame |
| 1 | `0x0002` | `FLAG_RESYNC` | transport | discard interpolation state; this Keyframe re-seeds it |
| 2 | `0x0004` | `FLAG_NODE_ONLY` | **canonical** | produced under the `node` profile (§5) |
| 3 | `0x0008` | `FLAG_END_OF_RUN` | **canonical** | last canonical frame at the end of the run |
| 4 | `0x0010` | `FLAG_CONTINUED` | transport | one of several frames carrying one logical unit (`WorldChunk`) |
| 5 | `0x0020` | `FLAG_SEEK_RESULT` | transport | this Keyframe answers a `run.seek` |
| 6–15 | — | reserved | — | MUST be 0 |

`CANONICAL_FLAG_MASK = 0x000C`, `TRANSPORT_FLAG_MASK = 0x0033`. §7.2 depends on this split.

### 2.4 Message types

| Id | Name | Direction | Cadence | Body §|
|---|---|---|---|---|
| `0x0001` | `Hello` | S→C | once per connection, first frame | §3.1 |
| `0x0002` | `Keyframe` | S→C | every `keyframe_period_ns` (default 1 s sim), plus resync/seek | §3.3 |
| `0x0003` | `Delta` | S→C | every `mobility_step_ns` (default 100 ms sim) | §3.4 |
| `0x0004` | `Telemetry` | S→C | every `telemetry_period_ns` (default 1 s sim), subscribed nodes only | §3.5 |
| `0x0005` | `Event` | S→C | one batch per mobility step, subscribed channels only | §3.6 |
| `0x0006` | `MetricSample` | S→C | every `metric_period_ns` (default 1 s sim) | §3.7 |
| `0x0007` | `Provenance` | S→C | after `Hello`, then on demand (`explain`, new model instances) | §3.8 |
| `0x0008` | `WorldChunk` | S→C | only when `world_ref.mode = 1` (static/WASM hosting) | §3.9 |
| `0x00FE` | `Error` | S→C | on error | §3.10 |
| `0x00FF` | `Bye` | S→C | once, last frame | §3.11 |

Ids `0x0009`–`0x00FD` and `0x0100`–`0xFFFF` are reserved. Client→server binary frames do not exist in v1.

`seq` accounting: canonical frames (`0x0002`–`0x0008`) consume `seq` values 0,1,2,…; `Hello`, `Error` and
`Bye` carry `seq` = the seq the **next** canonical frame will have, and do not consume one.

### 2.5 The symbol table

Strings on the wire are interned. A **string id** is a `u32` index into a per-connection, append-only
symbol table.

- Id `0` is always the empty string.
- `Hello` establishes ids `0..n-1`.
- Each `Provenance` frame MAY append entries; its own table's first entry takes the id equal to the
  current table size. Ids are never reassigned within a connection.
- A non-resumed `Hello` resets the table.

Serialised **StrTable** layout (4-byte aligned start):

| @ | Size | Type | Field |
|---|---|---|---|
| 0 | 4 | `u32` | `n` — number of strings |
| 4 | 4 | `u32` | `blob_bytes` — UTF-8 bytes, excluding padding |
| 8 | `4(n+1)` | `u32[n+1]` | `offsets` — `offsets[0] = 0`, `offsets[n] = blob_bytes`, non-decreasing |
| 8+4(n+1) | `blob_bytes` | `u8[]` | UTF-8 blob, zero-padded to a multiple of 4 |

String `i` is `blob[offsets[i] .. offsets[i+1]]`. Total section size is
`8 + 4(n+1) + ceil4(blob_bytes)`.

### 2.6 Compression

**`DECISION`: zstd (RFC 8878) is the only compression on the wire, because the MCAP recording already uses
zstd (ADR 0008) so one decoder covers live and replay.** Rust: `zstd` crate. TypeScript: `fzstd` (MIT,
pure JS, ~10 KB) or a WASM build — the client declares support by *not* passing `compress=none`.

Rules:

- `Hello` is never compressed.
- The server SHOULD compress a body only when `body_len >= 4096`; below that the ratio does not pay for
  the allocation.
- Compression level 3 (zstd default) — **`DECISION`: level 3 keeps encode under 1 ms for a 300 KB
  keyframe, which is the whole point of not blocking the sim loop.**
- `FLAG_COMPRESSED` is a **transport** flag: it is not part of the recorded bytes (§7.2).

---

## 3. Message layouts

### 3.0 Encoding choice

**`DECISION`: v1 uses a hand-rolled, flat, fixed-layout struct-of-arrays encoding, not FlatBuffers.**

ADR 0008 chose FlatBuffers. For v1 that is the wrong trade, for four reasons, and the ADR is amended here:

1. **Zero parsing, not just zero copy.** A FlatBuffers vector still costs a vtable indirection and a bounds
   check per field access, and a vector-of-tables costs one offset read per element. A fixed
   struct-of-arrays lets both sides do `bytemuck::cast_slice::<u8, i32>(&body[off..off+4*n])` /
   `new Int32Array(frame, 24 + off, n)` — the pose columns land in the `SharedArrayBuffer` rings of
   09-ui §2 with a single `set()` and no per-actor JS work, which is exactly the hot path the UI budget
   (09-ui §4) is built around.
2. **Two implementations stay in lockstep more easily with one table than with one compiler.** A `.fbs`
   schema requires `flatc` in the Rust build, the pnpm build and the Python build, pinned to the same
   version, plus generated code checked in or generated thrice. A 24-byte header and ten offset tables are
   reviewable in a diff.
3. **The recorder stores exactly these bytes** (§7.2), so there is one layout, not a wire format plus a
   storage format.
4. **We do not need FlatBuffers' evolution model.** §8 gives a narrower, sufficient one: reserved bytes,
   `record_size`, and new `off_*` sections.

What we give up: optional fields, unions, and free-form nesting. Those are used only in `Hello`,
`Provenance` and `Error`, which are cold and use the symbol table instead. Arbitrary structured data
(inspector snapshots, scenarios, manifests) travels as JSON over JSON-RPC, where it belongs.

### 3.1 `Hello` (`0x0001`)

Body = **fixed prefix (256 B)** ‖ node table ‖ class table ‖ channel table ‖ world ref ‖ symbol table.

#### 3.1.1 Prefix (256 bytes)

| @ | Size | Type | Field | Notes |
|---|---|---|---|---|
| 0 | 2 | `u16` | `version_major` | `1` |
| 2 | 2 | `u16` | `version_minor` | `0` |
| 4 | 4 | `u32` | `hello_flags` | §3.1.2 |
| 8 | 16 | `u8[16]` | `run_id` | UUIDv7, raw big-endian bytes as in RFC 9562 |
| 24 | 32 | `u8[32]` | `scenario_hash` | SHA-256 of the canonical scenario JSON (02-architecture §6.5) |
| 56 | 32 | `u8[32]` | `world_hash` | `World.content_hash` (03-interfaces §2) |
| 88 | 8 | `i64` | `t0_wall_ns` | Unix epoch ns, UTC, of sim time 0 (`scenario.time.t0`) |
| 96 | 8 | `u64` | `sim_duration_ns` | scenario `duration_s` in ns |
| 104 | 8 | `u64` | `mobility_step_ns` | `Δt_mob`, default `100_000_000` |
| 112 | 8 | `u64` | `keyframe_period_ns` | default `1_000_000_000` |
| 120 | 8 | `u64` | `telemetry_period_ns` | default `1_000_000_000` |
| 128 | 8 | `u64` | `metric_period_ns` | default `1_000_000_000` |
| 136 | 8 | `u64` | `resume_seq` | seq of the next canonical frame |
| 144 | 8 | `u64` | `sim_time_ns` | current stream position in sim time |
| 152 | 8 | `f64` | `origin_lat_deg` | WGS-84 latitude of the ENU origin |
| 160 | 8 | `f64` | `origin_lon_deg` | WGS-84 longitude |
| 168 | 8 | `f64` | `origin_alt_m` | ellipsoidal height, metres |
| 176 | 8 | `f64` | `bbox_min_x_m` | world bbox, ENU metres |
| 184 | 8 | `f64` | `bbox_min_y_m` | |
| 192 | 8 | `f64` | `bbox_max_x_m` | |
| 200 | 8 | `f64` | `bbox_max_y_m` | |
| 208 | 4 | `u32` | `actor_capacity` | max concurrent actor slots for the run; a preallocation hint |
| 212 | 4 | `u32` | `node_count` | rows in the node table |
| 216 | 2 | `u16` | `class_count` | rows in the class table |
| 218 | 2 | `u16` | `channel_count` | rows in the channel table |
| 220 | 4 | `u32` | `off_nodes` | |
| 224 | 4 | `u32` | `off_classes` | |
| 228 | 4 | `u32` | `off_channels` | |
| 232 | 4 | `u32` | `off_world_ref` | |
| 236 | 4 | `u32` | `off_strings` | |
| 240 | 4 | `u32` | `str_engine_version` | e.g. `"v2xw 0.4.0+9f0649d"` (version + git commit) |
| 244 | 4 | `u32` | `str_scenario_name` | `scenario.meta.name` |
| 248 | 4 | `u32` | `str_run_label` | human label, may be `""` |
| 252 | 4 | `u32` | `str_session_token` | opaque token echoed on `?resume=`; `""` on loopback |

#### 3.1.2 `hello_flags`

| Bit | Mask | Name | Meaning |
|---|---|---|---|
| 0 | `0x0000_0001` | `HELLO_LIVE` | stream is produced by a live engine |
| 1 | `0x0000_0002` | `HELLO_REPLAY` | stream is produced by the replay reader from an MCAP file |
| 2 | `0x0000_0004` | `HELLO_NODE_ONLY` | connection is in the `node` profile (§5) |
| 3 | `0x0000_0008` | `HELLO_WORLD_INLINE` | world arrives as `WorldChunk` frames, not over HTTP |
| 4 | `0x0000_0010` | `HELLO_PAUSED` | run exists but is not advancing |
| 5 | `0x0000_0020` | `HELLO_RESUMED` | this connection resumed an existing stream at `resume_seq` |
| 6 | `0x0000_0040` | `HELLO_SEEKABLE` | `run.seek` is available (always set for replay; set for live once ≥ 1 keyframe is recorded) |
| 7 | `0x0000_0080` | `HELLO_WRITABLE` | control methods that mutate the run are permitted on this connection |

Exactly one of `HELLO_LIVE` / `HELLO_REPLAY` MUST be set.

#### 3.1.3 Node table (`node_count = N`, 32·N bytes, 4-aligned)

Columns in this order, each a contiguous array of length `N`:

| # | Type | Column | Notes |
|---|---|---|---|
| 1 | `u32[N]` | `node_id` | ascending, dense where possible (03-interfaces §1) |
| 2 | `u32[N]` | `actor_id` | `0xFFFFFFFF` if the node is not mounted on an actor |
| 3 | `f32[N]` | `pos_x_m` | static nodes: fixed position; mobile nodes: position at `t0` |
| 4 | `f32[N]` | `pos_y_m` | |
| 5 | `f32[N]` | `pos_z_m` | antenna height included |
| 6 | `u32[N]` | `str_label` | e.g. `"veh_0421"`, `"rsu_north"` |
| 7 | `u32[N]` | `str_profile_id` | hardware profile id, e.g. `"obu/cohda-mk5"` (06-node-models §7) |
| 8 | `u16[N]` | `flags` | bit0 `HAS_HSM`, bit1 `IS_ATTACKER` **[GT]**, bit2 `IS_BACKEND`, bit3 `HAS_BACKHAUL`, bit4 `HAS_UU`, bits 5–15 reserved |
| 9 | `u8[N]` | `kind` | `0` obu, `1` vru-device, `2` rsu, `3` base-station, `4` router, `5` backend-entity, `6` other |
| 10 | `u8[N]` | `class_idx` | index into the class table, `0xFF` if not an actor |

Nodes that appear mid-run are announced on `sec.cert`/`gt.spawn` and in `Delta.spawns`; the node table is
the set known at connect time.

#### 3.1.4 Actor-class table (`class_count = C`, 24·C bytes, 4-aligned)

| # | Type | Column | Notes |
|---|---|---|---|
| 1 | `u32[C]` | `str_name` | `"car"`, `"truck"`, `"bus"`, `"moto"`, `"bicycle"`, `"pedestrian"`, … |
| 2 | `f32[C]` | `length_m` | `Dims` (03-interfaces §1) |
| 3 | `f32[C]` | `width_m` | |
| 4 | `f32[C]` | `height_m` | |
| 5 | `u32[C]` | `color_rgba` | renderer hint, `0xRRGGBBAA` as a big-endian-looking literal stored LE |
| 6 | `u16[C]` | `reserved16` | 0 |
| 7 | `u8[C]` | `category` | `0` vehicle, `1` vru, `2` infrastructure, `3` other |
| 8 | `u8[C]` | `reserved8` | 0 |

#### 3.1.5 Channel table (`channel_count = K`, 8·K bytes, 4-aligned)

| # | Type | Column | Notes |
|---|---|---|---|
| 1 | `u32[K]` | `str_id` | channel name exactly as in 03-interfaces §14, e.g. `"node.tx"` |
| 2 | `u16[K]` | `channel_id` | numeric id used in `Event` (§3.6.2) |
| 3 | `u8[K]` | `visibility` | `0` GT, `1` NODE, `2` PUBLIC, `3` MIXED, `4` DERIVED, `5` META |
| 4 | `u8[K]` | `enabled` | `1` if the server will emit it now (see `events.set`) |

The table lists every channel the server *can* emit on this connection. In the `node` profile, channels
with visibility `GT` MUST be omitted entirely.

#### 3.1.6 World reference (16 bytes)

| @ | Size | Type | Field | Notes |
|---|---|---|---|---|
| 0 | 1 | `u8` | `mode` | `0` HTTP GET by content hash (**default**), `1` inline `WorldChunk` frames, `2` client already has it |
| 1 | 1 | `u8` | `format` | `0` `vwp-world/1` binary (`.vwb`), `1` `vwp-world/1` JSON |
| 2 | 2 | `u16` | `reserved` | 0 |
| 4 | 4 | `u32` | `payload_bytes` | total size of the world payload |
| 8 | 4 | `u32` | `str_url` | path or absolute URL, e.g. `"/world/<64 hex>.vwb"` |
| 12 | 4 | `u32` | `reserved32` | 0 |

**`DECISION`: the world is fetched over HTTP GET by content hash, not streamed.** It is immutable, it is
large (a 1 km² downtown is 1–5 MB), it is shared between runs, and the browser cache plus
`immutable` + `ETag` makes a second run of the same world free. Streaming it would serialise the world
behind the telemetry stream and re-send it on every reconnect. `mode = 1` (`WorldChunk`, §3.9) exists only
for static/WASM hosting where there is no engine HTTP server (09-ui §9).

#### 3.1.7 Symbol table

A `StrTable` (§2.5) at `off_strings`. Establishes string ids `0..n-1`.

---

### 3.2 Pose quantisation (normative, used by `Keyframe` and `Delta`)

| Quantity | Wire type | Scale | Range | Resolution |
|---|---|---|---|---|
| absolute x, y | `i32` | millimetres, relative to `Keyframe.origin_{x,y}_m` | ±2,147,483 m | 1 mm |
| absolute z | `i16` | centimetres, relative to `Keyframe.origin_z_m` | ±327.67 m | 1 cm |
| delta x, y, z | `i16` | millimetres, relative to the same field in the **previously delivered frame of the GOP** | ±32.767 m per step | 1 mm |
| heading | `u16` | binary radians (brad): `rad = brad · 2π / 65536` | full turn | 0.0055° |
| speed | `i16` | 1/128 m/s | ±255.99 m/s | 7.8 mm/s |
| acceleration | `i16` | 1/64 m/s² | ±511.98 m/s² | 15.6 mm/s² |
| signal time-to-change | `u16` | deciseconds | 0–6553.4 s | 100 ms |

**`DECISION`: absolute positions are `i32` millimetres relative to a per-keyframe origin, *not* `i16`
millimetres.** ADR 0008 says "int16 millimetres on a per-keyframe origin", which is exactly right for
*deltas* (an actor at 130 km/h moves 3.6 m per 100 ms step, well inside ±32.767 m) but impossible for
*keyframes*: `i16` mm spans ±32.767 m, so it cannot address a 1 km² world from one origin. The ADR's
intent — 1 mm precision and ~10 bytes of pose per actor — is preserved: a keyframe row is 28 B raw
(≈ 280 KB for 10,000 actors, ≈ 60–90 KB after zstd, comfortably inside §7.1's 4 MiB chunk target) and a delta
row is 20 B.

**`DECISION`: world geometry is `f32` ENU metres; actor poses are `i32` millimetres.** `f32` has a 24-bit
mantissa, so at 1 km from the origin its ulp is 1000·2⁻²³ ≈ **0.12 mm**, and at 10 km ≈ 1.2 mm. The often
quoted "f32 gives 0.1 m" applies to *projected absolute* coordinates (UTM eastings ≈ 5×10⁵ m → ulp ≈ 6 cm),
which we never put on the wire: everything is world-local ENU about a `f64` geodetic origin carried in
`Hello`. `f32` is therefore ample for geometry a renderer consumes, and integer millimetres are used for
poses so that quantisation is exact and reproducible rather than rounding-mode dependent.

**`DECISION`: heading is `u16` binary radians, not `f32` radians and not `i16`.** Binary radians wrap
correctly by construction (no ±π branch), cost 2 bytes, and 0.0055° is two orders of magnitude finer than
anything a renderer or a plausibility detector needs. Unsigned removes the sign-extension question.

Normative quantisation rule (both endpoints MUST implement it identically):

```
q(v, scale) = clamp(round_half_away_from_zero(v * scale), TYPE_MIN, TYPE_MAX)
heading_brad = ((round_half_away_from_zero(rad * 65536 / (2π)) % 65536) + 65536) % 65536
```

**Delta reference rule.** A `Delta` position field is the difference against the value **as previously
transmitted and quantised**, never against the engine's unquantised state. The server keeps the quantised
pose of each slot as its reference. Consequence: quantisation error does not accumulate across a GOP, and
a client that applies keyframe + all deltas in order reproduces the server's quantised state exactly.

**Escape hatch.** If a step's `|dx|`, `|dy|` or `|dz|` would exceed 32,000 mm (teleport, `run.seek`,
mobility command, a spawn-adjacent jump), the server MUST set `MFLAG_ABSOLUTE` on that row and put the
absolute pose in the delta's absolute block (§3.4.3) instead. There is no failure mode.

---

### 3.3 `Keyframe` (`0x0002`)

Body = **prefix (64 B)** ‖ actor block ‖ signal block.

#### 3.3.1 Prefix (64 bytes)

| @ | Size | Type | Field | Notes |
|---|---|---|---|---|
| 0 | 8 | `u64` | `sim_time_ns` | a mobility-step boundary |
| 8 | 8 | `f64` | `origin_x_m` | quantisation origin |
| 16 | 8 | `f64` | `origin_y_m` | |
| 24 | 8 | `f64` | `origin_z_m` | |
| 32 | 4 | `u32` | `actor_count` | = slot high-water mark + 1; rows are indexed **by slot** |
| 36 | 4 | `u32` | `signal_count` | |
| 40 | 4 | `u32` | `off_actors` | |
| 44 | 4 | `u32` | `off_signals` | |
| 48 | 4 | `u32` | `gop_index` | keyframe ordinal from 0; deltas quote it |
| 52 | 2 | `u16` | `profile` | `0` full, `1` node-only |
| 54 | 2 | `u16` | `reserved` | 0 |
| 56 | 8 | `u64` | `reserved64` | 0 |

**`DECISION`: `origin_{x,y,z}_m` is constant for the whole run and equals `floor(bbox_min)` per axis
(`origin_z_m = 0`).** `i32` mm reaches ±2,147 km from it, so re-centring is never needed; the field stays
per-keyframe so a later minor version can re-centre without a format change. A client MUST read it from
each keyframe rather than caching it from `Hello`.

**`DECISION`: actor rows are a dense array indexed by slot, not a list keyed by actor id.** The UI writes
poses into `SharedArrayBuffer` rings "keyed by actor slot" (09-ui §2); a dense array makes that a
`TypedArray.set()`. Empty slots carry `actor_id = 0xFFFFFFFF` and zeros elsewhere. Slots are assigned by
the server at spawn as the lowest free slot (deterministic), and are released one full keyframe period
after despawn so that a late delta cannot be misapplied.

#### 3.3.2 Actor block (`actor_count = A`, 28·A bytes, 4-aligned)

| # | Type | Column | Unit / meaning | Vis |
|---|---|---|---|---|
| 1 | `u32[A]` | `actor_id` | `ActorId`; `0xFFFFFFFF` = empty slot | PUBLIC |
| 2 | `i32[A]` | `x_mm` | mm east of `origin_x_m` | PUBLIC |
| 3 | `i32[A]` | `y_mm` | mm north of `origin_y_m` | PUBLIC |
| 4 | `u32[A]` | `lane_id` | `LaneId`, `0xFFFFFFFF` = off-lane/unknown | **GT** |
| 5 | `i16[A]` | `z_cm` | cm up from `origin_z_m` | PUBLIC |
| 6 | `u16[A]` | `heading_brad` | binary radians, ENU, 0 = east, CCW | PUBLIC |
| 7 | `i16[A]` | `speed_cq` | 1/128 m/s along `heading` | PUBLIC |
| 8 | `i16[A]` | `accel_cq` | 1/64 m/s², longitudinal | **GT** |
| 9 | `u8[A]` | `class_idx` | index into the class table | PUBLIC |
| 10 | `u8[A]` | `state` | §3.3.4 | mixed |
| 11 | `u8[A]` | `verified_neighbors` | count of neighbours in state *verified*, saturating at 255 | NODE |
| 12 | `u8[A]` | `flags8` | reserved, 0 | — |

#### 3.3.3 Signal block (`signal_count = S`, 8·S bytes, 4-aligned)

| # | Type | Column | Meaning |
|---|---|---|---|
| 1 | `u32[S]` | `signal_id` | `SignalId` |
| 2 | `u16[S]` | `time_to_change_ds` | deciseconds to the next phase change, `0xFFFF` unknown |
| 3 | `u8[S]` | `phase` | SAE J2735 `MovementPhaseState`: `0` unavailable, `1` dark, `2` stop-then-proceed, `3` stop-and-remain, `4` pre-movement, `5` permissive-movement-allowed, `6` protected-movement-allowed, `7` permissive-clearance, `8` protected-clearance, `9` caution-conflicting-traffic |
| 4 | `u8[S]` | `reserved` | 0 |

Signals are PUBLIC: a vehicle sees the head with its eyes.

#### 3.3.4 The `state` byte

| Bit | Mask | Name | Meaning | Vis |
|---|---|---|---|---|
| 0 | `0x01` | `ST_ATTACKER` | the actor is running an `Attacker` plug-in right now | **GT** |
| 1 | `0x02` | `ST_REPORTED` | ≥ 1 misbehaviour report naming this actor has reached the MA | NODE |
| 2 | `0x04` | `ST_REVOKED` | on a published CRL / blocklist (05-protocols stage `published`) | PUBLIC |
| 3 | `0x08` | `ST_EQUIPPED` | carries an OBU or VRU device | PUBLIC |
| 4 | `0x10` | `ST_TRANSMITTING` | transmitted at least once in the last mobility step | NODE |
| 5 | `0x20` | `ST_PARKED` | parked / radio off (06-node-models §2.5) | PUBLIC |
| 6 | `0x40` | `ST_GNSS_DEGRADED` | GNSS fix worse than 3D, or an active outage/jam | NODE |
| 7 | `0x80` | `ST_WARNING_ACTIVE` | a safety application on this node is warning | NODE |

This is the "benign / attacker / reported / revoked" state the palette of 09-ui §10 renders: benign =
none of bits 0–2 set.

---

### 3.4 `Delta` (`0x0003`)

Body = **prefix (64 B)** ‖ moved ‖ absolute ‖ lanes ‖ spawns ‖ despawns ‖ signals.

A `Delta` is meaningful **only** when applied to the state produced by its GOP's `Keyframe` followed by
every earlier `Delta` of the same GOP in `step_index` order. A client that sees a `seq` gap, or a
`gop_index` it did not receive a keyframe for, MUST drop deltas until the next `Keyframe`.

#### 3.4.1 Prefix (64 bytes)

| @ | Size | Type | Field |
|---|---|---|---|
| 0 | 8 | `u64` | `sim_time_ns` |
| 8 | 4 | `u32` | `gop_index` — must equal the current keyframe's |
| 12 | 4 | `u32` | `step_index` — 1-based within the GOP |
| 16 | 4 | `u32` | `moved_count` (M) |
| 20 | 4 | `u32` | `abs_count` |
| 24 | 4 | `u32` | `lane_count` |
| 28 | 4 | `u32` | `spawn_count` (P) |
| 32 | 4 | `u32` | `despawn_count` (D) |
| 36 | 4 | `u32` | `signal_count` (S) |
| 40 | 4 | `u32` | `off_moved` |
| 44 | 4 | `u32` | `off_abs` |
| 48 | 4 | `u32` | `off_lanes` |
| 52 | 4 | `u32` | `off_spawns` |
| 56 | 4 | `u32` | `off_despawns` |
| 60 | 4 | `u32` | `off_signals` |

#### 3.4.2 Moved block (20·M bytes, 4-aligned)

| # | Type | Column | Meaning | Vis |
|---|---|---|---|---|
| 1 | `u32[M]` | `slot` | strictly ascending | PUBLIC |
| 2 | `i16[M]` | `dx_mm` | change since the previous frame of the GOP | PUBLIC |
| 3 | `i16[M]` | `dy_mm` | | PUBLIC |
| 4 | `i16[M]` | `dz_mm` | | PUBLIC |
| 5 | `u16[M]` | `heading_brad` | **absolute** | PUBLIC |
| 6 | `i16[M]` | `speed_cq` | **absolute**, 1/128 m/s | PUBLIC |
| 7 | `i16[M]` | `accel_cq` | **absolute**, 1/64 m/s² | **GT** |
| 8 | `u8[M]` | `state` | absolute, §3.3.4 | mixed |
| 9 | `u8[M]` | `verified_neighbors` | absolute | NODE |
| 10 | `u8[M]` | `mflags` | §3.4.2.1 | — |
| 11 | `u8[M]` | `reserved` | 0 | — |

Only actors whose quantised pose, `state`, `verified_neighbors` or lane changed appear. Heading, speed,
acceleration, state and neighbour count are absolute because they are already 1–2 bytes: delta-coding them
would save nothing and cost a reference.

##### 3.4.2.1 `mflags`

| Bit | Mask | Name | Meaning |
|---|---|---|---|
| 0 | `0x01` | `MFLAG_ABSOLUTE` | ignore `dx/dy/dz`; this row has an entry in the absolute block |
| 1 | `0x02` | `MFLAG_LANE_CHANGED` | this row has an entry in the lane block |
| 2–7 | — | reserved | 0 |

#### 3.4.3 Absolute block (12·`abs_count` bytes, 4-aligned)

Entries appear in the same order as the moved rows that set `MFLAG_ABSOLUTE`, as an **array of structs**:

| @ | Size | Type | Field |
|---|---|---|---|
| 0 | 4 | `i32` | `x_mm` (relative to the GOP keyframe's origin) |
| 4 | 4 | `i32` | `y_mm` |
| 8 | 2 | `i16` | `z_cm` |
| 10 | 2 | `u16` | `reserved` = 0 |

#### 3.4.4 Lane block (4·`lane_count` bytes) — **GT**

`u32[lane_count] lane_id`, in the order of the moved rows that set `MFLAG_LANE_CHANGED`. Under the `node`
profile this block MUST be absent (`lane_count = 0`, `off_lanes = 0`) and `MFLAG_LANE_CHANGED` MUST be
clear.

#### 3.4.5 Spawn block (36·P bytes, 4-aligned)

| # | Type | Column | Meaning | Vis |
|---|---|---|---|---|
| 1 | `u32[P]` | `slot` | newly occupied slot | PUBLIC |
| 2 | `u32[P]` | `actor_id` | | PUBLIC |
| 3 | `u32[P]` | `node_id` | `0xFFFFFFFF` if unequipped | PUBLIC |
| 4 | `i32[P]` | `x_mm` | absolute, GOP keyframe origin | PUBLIC |
| 5 | `i32[P]` | `y_mm` | | PUBLIC |
| 6 | `u32[P]` | `lane_id` | | **GT** |
| 7 | `i16[P]` | `z_cm` | | PUBLIC |
| 8 | `u16[P]` | `heading_brad` | | PUBLIC |
| 9 | `i16[P]` | `speed_cq` | | PUBLIC |
| 10 | `u16[P]` | `cause` | `0` demand, `1` scenario-event, `2` respawn, `3` handover-in, `0xFFFF` unknown | **GT** |
| 11 | `u8[P]` | `class_idx` | | PUBLIC |
| 12 | `u8[P]` | `state` | | mixed |
| 13 | `u8[P]` | `verified_neighbors` | | NODE |
| 14 | `u8[P]` | `reserved` | 0 | — |

#### 3.4.6 Despawn block (8·D bytes, 4-aligned)

| # | Type | Column | Meaning | Vis |
|---|---|---|---|---|
| 1 | `u32[D]` | `slot` | | PUBLIC |
| 2 | `u16[D]` | `cause` | `0` trip-end, `1` left-map, `2` parked, `3` scenario-event, `4` error, `0xFFFF` unknown | **GT** |
| 3 | `u16[D]` | `reserved` | 0 | — |

#### 3.4.7 Signal block

Identical layout to §3.3.3. Only signals whose `phase` or `time_to_change_ds` changed appear.

---

### 3.5 `Telemetry` (`0x0004`)

Body = **prefix (32 B)** ‖ records.

**`DECISION`: telemetry is an array of structs with a wire-carried `record_size`, unlike poses which are
struct-of-arrays.** The HUD and the inspector read *one node at a time* (09-ui §5), the batch is small
(subscribed nodes only, tens not thousands), and AoS + `record_size` gives the cheapest forward-compatible
extension path (§8).

#### 3.5.1 Prefix (32 bytes)

| @ | Size | Type | Field |
|---|---|---|---|
| 0 | 8 | `u64` | `sim_time_ns` — end of the sampling window |
| 8 | 8 | `u64` | `window_ns` — length of the sampling window |
| 16 | 4 | `u32` | `node_count` (N) |
| 20 | 4 | `u32` | `off_records` — MUST be 8-aligned |
| 24 | 4 | `u32` | `record_size` — **208** in v1 |
| 28 | 4 | `u32` | `reserved` = 0 |

#### 3.5.2 `NodeTelemetry` record — 208 bytes

Complete realisation of 06-node-models §2.4 plus the HUD of 09-ui §5. All rate/count fields are measured
over `window_ns`.

| @ | Size | Type | Field | Unit | Vis |
|---|---|---|---|---|---|
| 0 | 8 | `u64` | `storage_used_b` | bytes | NODE |
| 8 | 8 | `u64` | `storage_total_b` | bytes (profile capacity) | NODE |
| 16 | 8 | `u64` | `next_topup_ns` | sim time of the next certificate top-up; `u64::MAX` = none | NODE |
| 24 | 8 | `u64` | `crl_bytes` | bytes held in the CRL store | NODE |
| 32 | 8 | `u64` | `outbox_bytes` | bytes pending in the report outbox | NODE |
| 40 | 8 | `i64` | `clock_offset_ns` | believed − true time | **GT** |
| 48 | 4 | `u32` | `node_id` | | PUBLIC |
| 52 | 4 | `u32` | `ram_used_kib` | KiB (stores + queues + profile baseline) | NODE |
| 56 | 4 | `u32` | `ram_total_kib` | KiB | NODE |
| 60 | 4 | `u32` | `drop_rx_overflow` | count | NODE |
| 64 | 4 | `u32` | `drop_verify_policy_skip` | count | NODE |
| 68 | 4 | `u32` | `drop_verify_overflow` | count | NODE |
| 72 | 4 | `u32` | `drop_tx_overflow` | count | NODE |
| 76 | 4 | `u32` | `drop_reassembly_timeout` | count | NODE |
| 80 | 4 | `u32` | `drop_crl_backlog` | count | NODE |
| 84 | 4 | `u32` | `cert_stored` | own certificates on disk | NODE |
| 88 | 4 | `u32` | `crl_entries` | entries | NODE |
| 92 | 4 | `u32` | `outbox_msgs` | pending reports | NODE |
| 96 | 4 | `u32` | `peer_cache_entries` | peer certificate cache size | NODE |
| 100 | 4 | `u32` | `p2pcd_requests` | count in window | NODE |
| 104 | 4 | `u32` | `full_cert_msgs` | messages sent carrying a full certificate | NODE |
| 108 | 4 | `f32` | `msgs_in_per_s` | 1/s (delivered to the stack) | NODE |
| 112 | 4 | `f32` | `msgs_out_per_s` | 1/s | NODE |
| 116 | 4 | `f32` | `verifications_per_s` | 1/s completed | NODE |
| 120 | 4 | `f32` | `verify_wait_p50_ms` | ms (enqueue → start) | NODE |
| 124 | 4 | `f32` | `verify_wait_p95_ms` | ms | NODE |
| 128 | 4 | `f32` | `gnss_hdop` | dimensionless | NODE |
| 132 | 4 | `f32` | `gnss_sigma_m` | m, 1σ horizontal, from the model's own noise parameters | NODE |
| 136 | 4 | `f32` | `clock_drift_ppm` | ppm | NODE |
| 140 | 4 | `f32` | `pos_error_m` | ‖belief − truth‖ horizontal | **GT** |
| 144 | 4 | `f32` | `airtime_ms_per_s` | ms of transmitted air time per second | NODE |
| 148 | 2 | `u16` | `cpu_util_pm` | per-mille busy, averaged over cores | NODE |
| 150 | 2 | `u16` | `hsm_util_pm` | per-mille | NODE |
| 152 | 2 | `u16` | `q_rx_p50` | messages | NODE |
| 154 | 2 | `u16` | `q_rx_p95` | messages | NODE |
| 156 | 2 | `u16` | `q_verify_p50` | messages | NODE |
| 158 | 2 | `u16` | `q_verify_p95` | messages | NODE |
| 160 | 2 | `u16` | `q_app_p50` | messages | NODE |
| 162 | 2 | `u16` | `q_app_p95` | messages | NODE |
| 164 | 2 | `u16` | `q_tx_p50` | frames | NODE |
| 166 | 2 | `u16` | `q_tx_p95` | frames | NODE |
| 168 | 2 | `u16` | `q_crl_p50` | tasks | NODE |
| 170 | 2 | `u16` | `q_crl_p95` | tasks | NODE |
| 172 | 2 | `u16` | `dcc_state` | `0` RELAXED, `1` ACTIVE_1, `2` ACTIVE_2, `3` ACTIVE_3, `4` RESTRICTIVE, `5` C-V2X congestion-control, `0xFFFF` n/a | NODE |
| 174 | 2 | `u16` | `cbr_pm` | channel busy ratio, per-mille | NODE |
| 176 | 2 | `i16` | `tx_power_cdbm` | centi-dBm (0.01 dB) | NODE |
| 178 | 2 | `u16` | `nbr_total` | neighbour table size | NODE |
| 180 | 2 | `u16` | `nbr_verified` | | NODE |
| 182 | 2 | `u16` | `nbr_unverified` | | NODE |
| 184 | 2 | `u16` | `nbr_revoked` | | NODE |
| 186 | 2 | `u16` | `cert_active` | currently valid own pseudonyms/ATs | NODE |
| 188 | 2 | `u16` | `crl_expansion_pm` | per-mille of the current i-period expansion done | NODE |
| 190 | 2 | `u16` | `unverified_ratio_pm` | per-mille delivered without verification | NODE |
| 192 | 1 | `u8` | `gnss_fix` | `0` none, `1` 2D, `2` 3D, `3` DGNSS, `4` RTK-float, `5` RTK-fix, `6` dead-reckoning | NODE |
| 193 | 1 | `u8` | `node_state` | `0` off, `1` booting, `2` active, `3` parked, `4` degraded, `5` down, `6` compromised | `6` is **GT** |
| 194 | 1 | `u8` | `verify_policy` | `0` verify-all, `1` on-demand, `2` prioritized | NODE |
| 195 | 1 | `u8` | `reserved8` | 0 | — |
| 196 | 12 | — | `reserved` | MUST be 0 | — |

Saturation: any `u16` counter at its maximum means "≥ 65535". Unknown/not-modelled at the current tier is
`0xFFFF` for `u16`, `0xFFFF_FFFF` for `u32`, `NaN` for `f32`.

---
### 3.6 `Event` (`0x0005`)

A batch of typed records on the channels of 03-interfaces §14.

Body = **prefix (32 B)** ‖ index ‖ payload region.

#### 3.6.1 Prefix (32 bytes)

| @ | Size | Type | Field |
|---|---|---|---|
| 0 | 8 | `u64` | `t_start_ns` — inclusive lower bound of the batch |
| 8 | 8 | `u64` | `t_end_ns` — inclusive upper bound |
| 16 | 4 | `u32` | `event_count` (E) |
| 20 | 4 | `u32` | `off_index` — MUST be 8-aligned |
| 24 | 4 | `u32` | `off_payloads` — MUST be 8-aligned |
| 28 | 4 | `u32` | `payload_bytes` |

Index block (16·E bytes, 8-aligned), struct-of-arrays, **sorted by `(sim_time_ns, channel_id)` ascending**:

| # | Type | Column | Meaning |
|---|---|---|---|
| 1 | `u64[E]` | `sim_time_ns` | |
| 2 | `u32[E]` | `payload_off` | byte offset **relative to `off_payloads`**; MUST be a multiple of 8 |
| 3 | `u16[E]` | `payload_len` | including the padding to 8 |
| 4 | `u16[E]` | `channel_id` | §3.6.2 |

Every payload starts 8-aligned within the payload region and is zero-padded to a multiple of 8. A reader
that does not know a `channel_id` skips it using `payload_len` — this is how new channels are added
without a version bump (§8).

#### 3.6.2 Channel ids

| Id | Channel (03-interfaces §14) | Visibility | Payload |
|---|---|---|---|
| 1 | `gt.kinematics` | GT | §3.6.11 |
| 2 | `gt.attack.action` | GT | §3.6.12 |
| 3 | `gt.spawn` | GT | (carried in `Delta.spawns`; id reserved for the recording) |
| 4 | `gt.despawn` | GT | (carried in `Delta.despawns`) |
| 10 | `node.tx` | NODE | §3.6.4 |
| 11 | `phy.rx` | MIXED (NODE + GT) | §3.6.5 |
| 12 | `mac.cbr` | NODE | §3.6.13 |
| 13 | `net.frag` | NODE | §3.6.14 |
| 14 | `node.verify` | NODE | §3.6.6 |
| 15 | `node.telemetry` | NODE | delivered as `Telemetry` frames, not as `Event` |
| 16 | `node.neighbor` | NODE | §3.6.15 |
| 20 | `sec.cert` | NODE | §3.6.7 |
| 21 | `proto.msg` | NODE | §3.6.16 |
| 22 | `proto.revocation` | PUBLIC | §3.6.10 |
| 30 | `det.observation` | NODE | §3.6.8 |
| 31 | `ma.report` | NODE | §3.6.17 |
| 32 | `ma.case` | NODE | §3.6.17 |
| 33 | `ma.decision` | NODE | §3.6.17 |
| 40 | `app.warning` | NODE | §3.6.9 |
| 50 | `metric.sample` | DERIVED | delivered as `MetricSample` frames |
| 60 | `snapshot.keyframe` | MIXED | delivered as `Keyframe` frames |
| 61 | `snapshot.delta` | MIXED | delivered as `Delta` frames |
| 70 | `manifest` | META | delivered as `Hello` |

Ids 1–999 are reserved for the core; 1000–65534 are for plug-in channels, announced in `Hello.channels`;
`0xFFFF` is reserved.

Shared conventions for payloads: `node_id`, `msg_id` are `u32`; `msg_id` is run-unique and joins `node.tx`
→ `phy.rx` → `node.verify` → `det.observation`. `digest` fields are `HashedId8` (the low 8 bytes of the
SHA-256 of the certificate, IEEE 1609.2). Times other than the index time are absolute `SimTime`.

#### 3.6.3 `MsgType` enum (used by several payloads)

`0` other, `1` BSM, `2` CAM, `3` DENM, `4` SPaT, `5` MAP, `6` PSM, `7` VAM, `8` CPM, `9` SRM, `10` SSM,
`11` WSA, `12` CRL, `13` MisbehaviorReport, `14` P2PCD-request, `15` P2PCD-response, `16` CTL/ECTL,
`17` provisioning-request, `18` provisioning-response.

#### 3.6.4 `node.tx` (channel 10) — 40 bytes

| @ | Size | Type | Field | Unit | Vis |
|---|---|---|---|---|---|
| 0 | 4 | `u32` | `node_id` | | NODE |
| 4 | 4 | `u32` | `msg_id` | run-unique | NODE |
| 8 | 4 | `u32` | `bytes_on_air` | bytes incl. PHY/MAC headers | NODE |
| 12 | 4 | `f32` | `airtime_ms` | ms | NODE |
| 16 | 2 | `u16` | `msg_type` | §3.6.3 | NODE |
| 18 | 2 | `i16` | `tx_power_cdbm` | centi-dBm | NODE |
| 20 | 2 | `u16` | `channel` | DSRC channel number (172/178/184) or C-V2X carrier index | NODE |
| 22 | 1 | `u8` | `mcs` | MCS index | NODE |
| 23 | 1 | `u8` | `access_category` | `0` AC_VO, `1` AC_VI, `2` AC_BE, `3` AC_BK | NODE |
| 24 | 1 | `u8` | `dcc_state` | as `Telemetry.dcc_state` | NODE |
| 25 | 1 | `u8` | `signer_id_type` | `0` digest, `1` certificate, `2` self, `3` chain | NODE |
| 26 | 2 | `u16` | `payload_bytes` | secured PDU payload | NODE |
| 28 | 8 | `u8[8]` | `pseudonym_digest` | HashedId8 of the signing certificate | NODE |
| 36 | 4 | `u32` | `cert_id` | internal certificate id, `0` none | NODE |

#### 3.6.5 `phy.rx` (channel 11) — 48 bytes

| @ | Size | Type | Field | Unit | Vis |
|---|---|---|---|---|---|
| 0 | 8 | `u64` | `t_start_ns` | frame arrival start | NODE |
| 8 | 8 | `u64` | `t_end_ns` | reception outcome decided | NODE |
| 16 | 4 | `u32` | `rx_node` | | NODE |
| 20 | 4 | `u32` | `tx_node` | **`0xFFFFFFFF` in the `node` profile** | **GT** |
| 24 | 4 | `u32` | `msg_id` | | NODE |
| 28 | 4 | `f32` | `rssi_dbm` | dBm | NODE |
| 32 | 4 | `f32` | `sinr_db` | dB; `NaN` at the abstract tier | NODE |
| 36 | 4 | `f32` | `distance_m` | true tx–rx distance; `NaN` in the `node` profile | **GT** |
| 40 | 1 | `u8` | `outcome` | `0` ok, `1` below-sensitivity, `2` collision, `3` capture-loss, `4` half-duplex, `5` sinr-fail, `6` crc-fail, `7` not-a-candidate | NODE |
| 41 | 1 | `u8` | `cause` | `LossCause`: `0` none, `1` path-loss, `2` shadowing, `3` fading, `4` interference, `5` hidden-terminal, `6` half-duplex, `7` dcc-gate, `8` queue-drop, `9` out-of-range | NODE |
| 42 | 1 | `u8` | `los_class` | `0` LOS, `1` NLOSb, `2` NLOSv, `3` NLOSt, `4` NLOSbv; `0xFF` in the `node` profile | **GT** |
| 43 | 1 | `u8` | `reserved` | 0 | — |
| 44 | 4 | `u32` | `reserved32` | 0 | — |

This is the mixed channel 03-interfaces §14 calls out ("the tx id is GT; exporters project it out for
NODE-only outputs").

#### 3.6.6 `node.verify` (channel 14) — 48 bytes

| @ | Size | Type | Field | Unit | Vis |
|---|---|---|---|---|---|
| 0 | 8 | `u64` | `t_enqueue_ns` | | NODE |
| 8 | 8 | `u64` | `t_start_ns` | `u64::MAX` if never started | NODE |
| 16 | 8 | `u64` | `t_done_ns` | `u64::MAX` if never completed | NODE |
| 24 | 4 | `u32` | `node_id` | | NODE |
| 28 | 4 | `u32` | `msg_id` | | NODE |
| 32 | 4 | `f32` | `cost_us` | charged service time, µs | NODE |
| 36 | 2 | `u16` | `primitive` | `0` other, `1` ecdsa-p256-verify, `2` ecdsa-p256-sign, `3` ecdsa-p384-verify, `4` ml-dsa-65-verify, `5` ml-dsa-65-sign, `6` falcon-512-verify, `7` slh-dsa-shake-128s-verify, `8` ecqv-p256-reconstruct, `9` sha-256, `10` aes-128-ccm | NODE |
| 38 | 1 | `u8` | `outcome` | `0` valid, `1` invalid-signature, `2` revoked, `3` expired, `4` unknown-signer, `5` skipped, `6` dropped, `7` permission-denied | NODE |
| 39 | 1 | `u8` | `policy_decision` | `Admit`: `0` now, `1` deferred, `2` skipped, `3` evicted | NODE |
| 40 | 1 | `u8` | `policy_reason` | `0` none, `1` not-relevant, `2` queue-full, `3` age, `4` low-priority, `5` duplicate, `6` already-verified-signer | NODE |
| 41 | 1 | `u8` | `where_run` | `0` cpu, `1` hsm, `2` accelerator | NODE |
| 42 | 2 | `u16` | `queue_depth_at_enqueue` | | NODE |
| 44 | 4 | `u32` | `reserved32` | 0 | — |

#### 3.6.7 `sec.cert` (channel 20) — 40 bytes

| @ | Size | Type | Field | Unit | Vis |
|---|---|---|---|---|---|
| 0 | 8 | `u64` | `valid_from_ns` | | NODE |
| 8 | 8 | `u64` | `valid_until_ns` | | NODE |
| 16 | 4 | `u32` | `node_id` | | NODE |
| 20 | 4 | `u32` | `cert_id` | | NODE |
| 24 | 8 | `u8[8]` | `digest` | HashedId8 | NODE |
| 32 | 1 | `u8` | `event` | `0` change, `1` expire, `2` topup-request, `3` topup-complete, `4` learn-p2pcd, `5` learn-full-cert, `6` install, `7` evict, `8` revoked-self-detected | NODE |
| 33 | 1 | `u8` | `cert_kind` | `0` pseudonym/AT, `1` enrollment, `2` identification, `3` application, `4` CA, `5` trust-anchor | NODE |
| 34 | 2 | `u16` | `index_i` | SCMS i-period (`0xFFFF` n/a) | NODE |
| 36 | 2 | `u16` | `index_j` | SCMS j index (`0xFFFF` n/a) | NODE |
| 38 | 2 | `u16` | `count` | certificates affected by this event | NODE |

#### 3.6.8 `det.observation` (channel 30) — 32 bytes

| @ | Size | Type | Field | Unit | Vis |
|---|---|---|---|---|---|
| 0 | 4 | `u32` | `node_id` | the observer | NODE |
| 4 | 4 | `u32` | `str_detector` | string id of the detector model id | NODE |
| 8 | 8 | `u8[8]` | `subject_digest` | HashedId8 of the accused pseudonym | NODE |
| 16 | 4 | `f32` | `score` | `[0,1]` | NODE |
| 20 | 4 | `u32` | `subject_actor_id` | the real actor behind the pseudonym; `0xFFFFFFFF` in the `node` profile | **GT** |
| 24 | 2 | `u16` | `evidence_count` | messages cited | NODE |
| 26 | 1 | `u8` | `detector_kind` | `0` plausibility, `1` consistency, `2` behavioural, `3` perception-crosscheck, `4` cryptographic, `5` other | NODE |
| 27 | 1 | `u8` | `reserved` | 0 | — |
| 28 | 4 | `u32` | `prov_id` | provenance id resolving the detector's model card (§3.8) | NODE |

#### 3.6.9 `app.warning` (channel 40) — 32 bytes

| @ | Size | Type | Field | Unit | Vis |
|---|---|---|---|---|---|
| 0 | 4 | `u32` | `node_id` | | NODE |
| 4 | 4 | `u32` | `str_app` | string id: `"fcw"`, `"eebl"`, `"ima"`, `"vru"`, … | NODE |
| 8 | 8 | `u8[8]` | `subject_digest` | HashedId8 of the pseudonym the warning is about | NODE |
| 16 | 4 | `f32` | `ttc_s` | time to collision, s; `NaN` if n/a | NODE |
| 20 | 4 | `f32` | `distance_m` | as believed by the node | NODE |
| 24 | 1 | `u8` | `kind` | `0` issue, `1` update, `2` clear | NODE |
| 25 | 1 | `u8` | `severity` | `0` info, `1` caution, `2` warning, `3` imminent | NODE |
| 26 | 1 | `u8` | `truth` | `0` unknown, `1` true-positive, `2` false-positive, `3` missed; `0` in the `node` profile | **GT** |
| 27 | 1 | `u8` | `reserved` | 0 | — |
| 28 | 4 | `u32` | `subject_actor_id` | `0xFFFFFFFF` in the `node` profile | **GT** |

#### 3.6.10 `proto.revocation` (channel 22) — 32 bytes — **PUBLIC**

One record per (revocation, stage) pair, so `revocation_latency_stage` (08-measurement §2.3) is a
difference of two records' index times.

| @ | Size | Type | Field | Unit | Vis |
|---|---|---|---|---|---|
| 0 | 4 | `u32` | `subject_node_id` | the device being revoked | PUBLIC |
| 4 | 4 | `u32` | `revocation_id` | run-unique, joins the stages | PUBLIC |
| 8 | 8 | `u8[8]` | `subject_digest` | HashedId8 or linkage seed digest | PUBLIC |
| 16 | 8 | `u64` | `size_bytes` | list size at this stage; `0` if n/a | PUBLIC |
| 24 | 4 | `u32` | `node_id` | node the per-node stage applies to; `0xFFFFFFFF` for global stages | PUBLIC |
| 28 | 1 | `u8` | `stage` | 05-protocols §8: `0` detect, `1` report_sent, `2` report_received, `3` decision, `4` resolved, `5` issued, `6` published, `7` downloaded, `8` processed, `9` enforced, `10` residual_harm | PUBLIC |
| 29 | 1 | `u8` | `mechanism` | `0` active-crl, `1` passive-starvation, `2` blocklist, `3` committee-list | PUBLIC |
| 30 | 2 | `u16` | `entries` | CRL/blocklist entry count; `0xFFFF` ≥ 65535 | PUBLIC |

Stages `0`–`3` name the subject before anything is public; the channel is nevertheless PUBLIC because a
revocation record is the *protocol's* output, not the world's ground truth, and it is what the residual-harm
metrics (08-measurement §2.3) are defined over. Under the `node` profile the server MUST withhold
stages `0`–`4` (see §5.3) and emit only `issued` and later.

#### 3.6.11 `gt.kinematics` (channel 1) — 56 bytes — **GT**

| @ | Size | Type | Field |
|---|---|---|---|
| 0 | 4 | `u32` | `actor_id` |
| 4 | 4 | `u32` | `lane_id` |
| 8 | 4 | `f32` | `pos_x_m` |
| 12 | 4 | `f32` | `pos_y_m` |
| 16 | 4 | `f32` | `pos_z_m` |
| 20 | 4 | `f32` | `vel_x_mps` |
| 24 | 4 | `f32` | `vel_y_mps` |
| 28 | 4 | `f32` | `vel_z_mps` |
| 32 | 4 | `f32` | `acc_x_mps2` |
| 36 | 4 | `f32` | `acc_y_mps2` |
| 40 | 4 | `f32` | `acc_z_mps2` |
| 44 | 4 | `f32` | `heading_rad` |
| 48 | 4 | `f32` | `yaw_rate_rad_s` |
| 52 | 4 | `f32` | `lane_s_m` |

#### 3.6.12 `gt.attack.action` (channel 2) — 32 bytes — **GT**

| @ | Size | Type | Field |
|---|---|---|---|
| 0 | 4 | `u32` | `actor_id` — the true attacker |
| 4 | 4 | `u32` | `node_id` |
| 8 | 4 | `u32` | `msg_id` — affected message, `0xFFFFFFFF` if none |
| 12 | 4 | `u32` | `str_attack_id` — attacker model id |
| 16 | 4 | `u32` | `fields_changed` — bitmask: `1` position, `2` speed, `4` heading, `8` accel, `16` time, `32` identity, `64` path-history, `128` payload |
| 20 | 4 | `f32` | `magnitude` — model-defined scalar (e.g. metres of offset) |
| 24 | 1 | `u8` | `action` — `0` falsify-outgoing, `1` use-credential, `2` suppress, `3` delay, `4` replay, `5` transmit-raw/jam, `6` forge-report, `7` rsu-broadcast, `8` coordinate |
| 25 | 1 | `u8` | `coalition_id` — `0xFF` none |
| 26 | 2 | `u16` | `reserved` |
| 28 | 4 | `u32` | `prov_id` |

#### 3.6.13 `mac.cbr` (channel 12) — 16 bytes

| @ | Size | Type | Field |
|---|---|---|---|
| 0 | 4 | `u32` | `node_id` |
| 4 | 4 | `f32` | `cbr` — `[0,1]` |
| 8 | 2 | `u16` | `channel` |
| 10 | 2 | `u16` | `dcc_state` |
| 12 | 2 | `i16` | `tx_power_cdbm` |
| 14 | 2 | `u16` | `reserved` |

#### 3.6.14 `net.frag` (channel 13) — 24 bytes

| @ | Size | Type | Field |
|---|---|---|---|
| 0 | 4 | `u32` | `node_id` |
| 4 | 4 | `u32` | `sdu_id` |
| 8 | 4 | `u32` | `sdu_bytes` |
| 12 | 2 | `u16` | `fragments_total` |
| 14 | 2 | `u16` | `fragments_received` |
| 16 | 2 | `u16` | `msg_type` |
| 18 | 1 | `u8` | `outcome` — `0` reassembled, `1` timeout, `2` missing-fragment, `3` buffer-full |
| 19 | 1 | `u8` | `direction` — `0` tx, `1` rx |
| 20 | 4 | `u32` | `reserved32` |

#### 3.6.15 `node.neighbor` (channel 16) — 32 bytes

| @ | Size | Type | Field |
|---|---|---|---|
| 0 | 4 | `u32` | `node_id` |
| 4 | 4 | `u32` | `reserved32` |
| 8 | 8 | `u8[8]` | `peer_digest` |
| 16 | 4 | `f32` | `relevance` — `[0,1]` |
| 20 | 2 | `u16` | `table_size_after` |
| 22 | 1 | `u8` | `op` — `0` insert, `1` update, `2` expire, `3` evict, `4` mark-revoked |
| 23 | 1 | `u8` | `verify_state` — `0` unverified, `1` verified, `2` failed, `3` revoked |
| 24 | 4 | `u32` | `peer_actor_id` — **GT**, `0xFFFFFFFF` in the `node` profile |
| 28 | 4 | `u32` | `prov_id` |

#### 3.6.16 `proto.msg` (channel 21) — 32 bytes

| @ | Size | Type | Field |
|---|---|---|---|
| 0 | 4 | `u32` | `from_node` |
| 4 | 4 | `u32` | `to_node` |
| 8 | 4 | `u32` | `str_flow` — flow id, e.g. `"scms.topup"` |
| 12 | 4 | `u32` | `bytes` |
| 16 | 4 | `u32` | `flow_instance_id` |
| 20 | 2 | `u16` | `step` — index within the flow's state machine |
| 22 | 1 | `u8` | `transport` — `0` backend-net, `1` uu-ul, `2` uu-dl, `3` rsu-broadcast, `4` rsu-unicast, `5` backhaul, `6` pc5 |
| 23 | 1 | `u8` | `outcome` — `0` sent, `1` delivered, `2` timeout, `3` rejected, `4` queued |
| 24 | 4 | `f32` | `latency_ms` — `NaN` for `sent` |
| 28 | 4 | `u32` | `prov_id` |

#### 3.6.17 `ma.report` / `ma.case` / `ma.decision` (channels 31/32/33) — 40 bytes

| @ | Size | Type | Field |
|---|---|---|---|
| 0 | 4 | `u32` | `case_id` — `0xFFFFFFFF` for a bare report |
| 4 | 4 | `u32` | `report_id` — `0xFFFFFFFF` for a case/decision record |
| 8 | 8 | `u8[8]` | `subject_digest` |
| 16 | 4 | `u32` | `reporter_node_id` |
| 20 | 4 | `f32` | `confidence` — `[0,1]` |
| 24 | 4 | `u32` | `str_pipeline` — MA pipeline model id |
| 28 | 4 | `u32` | `subject_actor_id` — **GT**, `0xFFFFFFFF` in the `node` profile |
| 32 | 2 | `u16` | `reports_in_case` |
| 34 | 1 | `u8` | `kind` — `0` report-received, `1` case-opened, `2` case-updated, `3` case-closed, `4` decision-revoke, `5` decision-no-action, `6` decision-investigate |
| 35 | 1 | `u8` | `reporter_kind` — `0` obu, `1` rsu, `2` backend |
| 36 | 4 | `u32` | `prov_id` |

---

### 3.7 `MetricSample` (`0x0006`)

Body = **prefix (32 B)** ‖ samples.

Prefix:

| @ | Size | Type | Field |
|---|---|---|---|
| 0 | 8 | `u64` | `sim_time_ns` — **end** of the time bin |
| 8 | 8 | `u64` | `bin_width_ns` |
| 16 | 4 | `u32` | `sample_count` (M) |
| 20 | 4 | `u32` | `off_samples` — MUST be 8-aligned |
| 24 | 4 | `u32` | `record_size` — **32** in v1 |
| 28 | 4 | `u32` | `reserved` = 0 |

Sample record (array of structs, 32 bytes):

| @ | Size | Type | Field | Meaning |
|---|---|---|---|---|
| 0 | 8 | `f64` | `value` | the aggregated value |
| 8 | 4 | `u32` | `str_metric` | string id of the `MetricDef.name`, e.g. `"pdr"` |
| 12 | 4 | `u32` | `dim_key` | handle into the dimension dictionary (§3.8); `0` = no dimensions |
| 16 | 4 | `u32` | `node_id` | `0xFFFFFFFF` if not per-node |
| 20 | 4 | `u32` | `count` | number of underlying observations |
| 24 | 2 | `u16` | `agg` | `0` sum, `1` mean, `2` p50, `3` p95, `4` p99, `5` ratio, `6` rate, `7` max, `8` min |
| 26 | 1 | `u8` | `visibility` | `0` GT, `1` NODE, `2` PUBLIC, `3` DERIVED |
| 27 | 1 | `u8` | `reserved` = 0 | |
| 28 | 4 | `u32` | `prov_id` | provenance id; `0` none |

Units are not on the wire: they come from the `MetricDef`, which the client gets from `metrics.query`
(§6.10) or `rpc.discover`. A sample with `visibility = 0` MUST NOT be emitted in the `node` profile.

---

### 3.8 `Provenance` (`0x0007`)

Resolves the `prov_id` values that appear in `MetricSample` and in several event payloads into
(model id, model version, parameter-set id), so the inspector's "why" tab (09-ui §5, 02-architecture §6.5)
can explain any displayed value without a round trip. It also carries the dimension dictionary used by
`MetricSample.dim_key`.

Body = **prefix (32 B)** ‖ entries ‖ dims ‖ symbol-table extension.

Prefix:

| @ | Size | Type | Field |
|---|---|---|---|
| 0 | 8 | `u64` | `sim_time_ns` |
| 8 | 4 | `u32` | `entry_count` (P) |
| 12 | 4 | `u32` | `off_entries` |
| 16 | 4 | `u32` | `dim_count` (Dk) |
| 20 | 4 | `u32` | `off_dims` |
| 24 | 4 | `u32` | `off_strings` — symbol-table extension, or `0` |
| 28 | 4 | `u32` | `flags` — bit0 `PROV_REPLACE_ALL`, bit1 `PROV_FINAL` (no more will be sent) |

Entry block (24·P bytes, struct-of-arrays):

| # | Type | Column | Meaning |
|---|---|---|---|
| 1 | `u32[P]` | `prov_id` | non-zero, unique for the run |
| 2 | `u32[P]` | `str_model_id` | e.g. `"radio/propagation/log-distance-shadowing"` |
| 3 | `u32[P]` | `str_model_version` | semver, e.g. `"1.2.0"` |
| 4 | `u32[P]` | `str_param_set_id` | content-addressed parameter-set id, e.g. `"b3:9f2c1e…"` |
| 5 | `u32[P]` | `str_card_url` | URL of the generated model-card page |
| 6 | `u16[P]` | `family` | index into the `family` enum of the model-card schema (03-interfaces §12), in the order listed there |
| 7 | `u16[P]` | `subject_kind` | `0` value, `1` node, `2` link, `3` actor, `4` metric, `5` channel, `6` world |

Dim block (8·Dk bytes):

| # | Type | Column | Meaning |
|---|---|---|---|
| 1 | `u32[Dk]` | `dim_key` | the handle referenced by `MetricSample.dim_key` |
| 2 | `u32[Dk]` | `str_dims` | canonical `"k=v,k=v"` with keys sorted ASCII-ascending, e.g. `"dist_bin=75,rat=dsrc"` |

Symbol-table extension: a `StrTable` (§2.5) whose entry `i` takes global id `table_size_before + i`.

The server MUST send at least one `Provenance` frame immediately after the first `Keyframe`, covering every
`prov_id` it will reference before the next one. `explain` (§6.9) returns the same information as JSON,
resolved and human-readable.

---

### 3.9 `WorldChunk` (`0x0008`)

Used only when `Hello.world_ref.mode = 1` (static/WASM hosting, no engine HTTP server). Carries the same
bytes an HTTP `GET /world/{hash}.vwb` would return, split into chunks of at most 1 MiB. All chunks but the
last carry `FLAG_CONTINUED`.

Body = **prefix (64 B)** ‖ payload.

| @ | Size | Type | Field |
|---|---|---|---|
| 0 | 32 | `u8[32]` | `world_hash` — MUST equal `Hello.world_hash` |
| 32 | 8 | `u64` | `total_bytes` |
| 40 | 4 | `u32` | `chunk_index` — 0-based |
| 44 | 4 | `u32` | `chunk_count` |
| 48 | 4 | `u32` | `off_payload` |
| 52 | 4 | `u32` | `payload_len` |
| 56 | 1 | `u8` | `format` — `0` `vwp-world/1` binary, `1` JSON |
| 57 | 3 | — | reserved = 0 |
| 60 | 4 | `u32` | `reserved32` = 0 |

The client MUST verify `SHA-256(concat(payloads)) == world_hash` and MUST discard the world on mismatch.

---

### 3.10 `Error` (`0x00FE`)

Body = **prefix (32 B)** ‖ symbol-table extension.

| @ | Size | Type | Field |
|---|---|---|---|
| 0 | 8 | `u64` | `sim_time_ns` |
| 8 | 4 | `i32` | `code` — the same numbering as the JSON-RPC error codes (§6.4) |
| 12 | 4 | `u32` | `off_strings` |
| 16 | 4 | `u32` | `str_message` — one line, human readable |
| 20 | 4 | `u32` | `str_detail` — may be a JSON document as a string |
| 24 | 1 | `u8` | `fatal` — `1` means a `Bye` follows and the socket closes |
| 25 | 3 | — | reserved = 0 |
| 28 | 4 | `u32` | `reserved32` = 0 |

`Error` reports *stream* problems (the engine aborted, a model failed, the resume window was lost).
Problems with a specific control call are reported as JSON-RPC errors on the text channel instead.

### 3.11 `Bye` (`0x00FF`)

| @ | Size | Type | Field |
|---|---|---|---|
| 0 | 8 | `u64` | `sim_time_ns` |
| 8 | 8 | `u64` | `canonical_frames` — total canonical frames produced for this run |
| 16 | 1 | `u8` | `reason` — `0` run-complete, `1` client-requested, `2` server-shutdown, `3` error, `4` superseded-by-reconnect |
| 17 | 3 | — | reserved = 0 |
| 20 | 4 | `u32` | `off_strings` |
| 24 | 4 | `u32` | `str_detail` |
| 28 | 4 | `u32` | `reserved32` = 0 |

Followed by a symbol-table extension at `off_strings` (may be an empty table). The server closes the
socket with 1000 after sending `Bye` (except reason `4`, which uses 1012).

---
## 4. The world payload — `vwp-world/1`

**`DECISION`: road geometry, lane polylines and building footprints are fetched with one HTTP GET by
content hash, in a flat binary format with the same conventions as §2.** Reason in §3.1.6.

```
GET /world/{64-hex-sha256}.vwb        → application/vnd.v2xw.world.v1
GET /world/{64-hex-sha256}.json       → application/json
```

Both are immutable and cacheable forever. A client that already holds the world for `Hello.world_hash`
skips the fetch entirely — this is what makes a second run of the same scenario start instantly.

### 4.1 File header (16 bytes)

| @ | Size | Type | Field |
|---|---|---|---|
| 0 | 4 | `u32` | `magic` = `0x444C5756` (wire bytes `56 57 4C 44` = `V W L D`) |
| 4 | 2 | `u16` | `version` = 1 |
| 6 | 2 | `u16` | `reserved` = 0 |
| 8 | 4 | `u32` | `body_len` — uncompressed body bytes |
| 12 | 2 | `u16` | `flags` — bit0 `zstd` |
| 14 | 2 | `u16` | `reserved` = 0 |

Body starts at file offset 16. All body offsets below are body-relative.

### 4.2 Directory prefix (192 bytes)

| @ | Size | Type | Field |
|---|---|---|---|
| 0 | 32 | `u8[32]` | `content_hash` — SHA-256 of the body **with these 32 bytes zeroed** (see the amendment below); MUST equal the URL hash and `Hello.world_hash` |
| 32 | 8 | `f64` | `origin_lat_deg` |
| 40 | 8 | `f64` | `origin_lon_deg` |
| 48 | 8 | `f64` | `origin_alt_m` |
| 56 | 8 | `f64` | `bbox_min_x_m` |
| 64 | 8 | `f64` | `bbox_min_y_m` |
| 72 | 8 | `f64` | `bbox_max_x_m` |
| 80 | 8 | `f64` | `bbox_max_y_m` |
| 88 | 4 | `f32` | `bbox_min_z_m` |
| 92 | 4 | `f32` | `bbox_max_z_m` |
| 96 | 4 | `u32` | `lane_count` |
| 100 | 4 | `u32` | `lane_point_total` |
| 104 | 4 | `u32` | `building_count` |
| 108 | 4 | `u32` | `ring_point_total` |
| 112 | 4 | `u32` | `off_lanes` |
| 116 | 4 | `u32` | `off_lane_points` |
| 120 | 4 | `u32` | `off_buildings` |
| 124 | 4 | `u32` | `off_ring_points` |
| 128 | 4 | `u32` | `junction_count` |
| 132 | 4 | `u32` | `off_junctions` |
| 136 | 4 | `u32` | `signal_count` |
| 140 | 4 | `u32` | `off_signals` |
| 144 | 4 | `u32` | `site_count` |
| 148 | 4 | `u32` | `off_sites` |
| 152 | 4 | `u32` | `off_strings` |
| 156 | 4 | `u32` | `crossing_count` |
| 160 | 4 | `u32` | `off_crossings` |
| 164 | 4 | `u32` | `landuse_count` |
| 168 | 4 | `u32` | `off_landuse` |
| 172 | 4 | `u32` | `off_provenance_json` |
| 176 | 4 | `u32` | `provenance_json_bytes` |
| 180 | 12 | — | reserved = 0 |

The `bbox` here is authoritative and MUST equal `Hello.bbox_*`.

**Amendment, 2026-09-18 — `content_hash` is computed with its own field zeroed.** As
first written, this row asked for "the SHA-256 of the body" while storing that digest
*inside* the body, which cannot hold literally: changing the field changes the body, and
therefore the digest of the body. The rule, which both existing implementations already
follow independently — the Rust writer (`v2xw-world/src/serde_vwp.rs`) and the TypeScript
client (`ui/packages/protocol`), whose digests agree byte for byte on the 2 MB Manhattan
payload — is:

> A writer MUST zero the 32 bytes at body offset 0, hash the **whole body** including
> those 32 zero bytes, and then write the digest into them. A reader MUST verify by
> copying the body, zeroing the same 32 bytes, and hashing the copy. Nothing else about
> the body changes.

Conformance item W1 (§10.5) is read with this rule: "a body whose SHA-256 equals `{hash}`"
means the body so zeroed. The two `MUST`s in the row above are unchanged: this digest is
the `{hash}` in `GET /world/{hash}.vwb` and the value `Hello.world_hash` carries.

**Recorded defect, 2026-09-18 — §3.1.1's gloss on `Hello.world_hash` contradicts this
row.** §3.1.1 annotates the field as "`World.content_hash` (03-interfaces §2)", which is
the engine's *geometry* digest: a hash of the quantised model, independent of any wire
format, and a different number from the payload digest defined here. §4.2's `MUST`, W1 and
§4's caching rule ("a client that already holds the world for `Hello.world_hash` skips the
fetch entirely" — a statement about a URL key) all require `Hello.world_hash` to be the
**payload** digest, and that is what an implementation MUST send; a server that sends the
geometry digest produces a `Hello` whose `world_hash` resolves to no payload. A future
revision should either rename one of the two fields or carry both, so the ambiguity cannot
be read into it again. The implementation keeps them apart by name: *geometry digest* for
`World::content_hash`, *payload digest* for `WorldPayload::content_hash`.

### 4.3 Lanes (36·`lane_count`, 4-aligned)

Struct-of-arrays:

| # | Type | Column | Meaning |
|---|---|---|---|
| 1 | `u32[L]` | `lane_id` | `LaneId` |
| 2 | `u32[L]` | `point_off` | index of the first centreline point in the point arrays |
| 3 | `u32[L]` | `point_count` | ≥ 2 |
| 4 | `u32[L]` | `edge_id` | `EdgeId` |
| 5 | `u32[L]` | `junction_id` | `0xFFFFFFFF` unless the lane is internal to a junction |
| 6 | `u32[L]` | `str_name` | street name, may be `""` |
| 7 | `f32[L]` | `width_m` | |
| 8 | `f32[L]` | `speed_limit_mps` | |
| 9 | `u16[L]` | `allowed_classes` | bitmask: `1` car, `2` truck, `4` bus, `8` moto, `16` bicycle, `32` pedestrian, `64` emergency, `128` rail |
| 10 | `u8[L]` | `lane_type` | `0` drive, `1` bike, `2` sidewalk, `3` bus, `4` parking, `5` junction-internal, `6` crossing |
| 11 | `u8[L]` | `index_in_edge` | 0 = rightmost in the direction of travel |

**Lane centrelines** (`off_lane_points`), three parallel `f32` arrays of length `lane_point_total`, 4-aligned:

```
f32[lane_point_total] x_m
f32[lane_point_total] y_m
f32[lane_point_total] z_m
```

Lane `i` uses `x_m[point_off[i] .. point_off[i]+point_count[i]]`. Points are in travel order, in ENU
metres relative to the geodetic origin. Successive points MUST differ by ≥ 1 mm.

**`DECISION`: lane connectivity (`successors`, conflict matrices) is NOT in the world payload.** The UI
never needs it — it renders geometry and follows engine-produced poses — and shipping the lane graph would
roughly double the payload. Tools that need it call `inspect.entity` (§6.8) or read the engine's world
cache directly.

### 4.4 Buildings (28·`building_count`, 4-aligned)

| # | Type | Column | Meaning |
|---|---|---|---|
| 1 | `u32[B]` | `building_id` | |
| 2 | `u32[B]` | `ring_off` | index of the first point of the outer ring |
| 3 | `u32[B]` | `ring_count` | number of points in the outer ring, ≥ 3 |
| 4 | `f32[B]` | `height_m` | above `base_z_m` |
| 5 | `f32[B]` | `base_z_m` | terrain height at the footprint |
| 6 | `u32[B]` | `str_name` | may be `""` |
| 7 | `u8[B]` | `material` | `0` unknown, `1` concrete, `2` brick, `3` glass, `4` wood, `5` metal |
| 8 | `u8[B]` | `lod_hint` | `0` box, `1` box+roof, `2` detailed |
| 9 | `u16[B]` | `levels` | storeys, `0xFFFF` unknown |

**Ring points** (`off_ring_points`), two parallel `f32` arrays of length `ring_point_total`:

```
f32[ring_point_total] x_m
f32[ring_point_total] y_m
```

Rings are **counter-clockwise**, **not closed** (the renderer closes them), and each point's z is the
building's `base_z_m`. **`DECISION`: v1 stores outer rings only; interior holes are dropped at import and
the drop is recorded in the world provenance.** Holes matter to neither the obstacle model at the tiers
we ship nor the extruded-footprint renderer, and supporting them would need a second indirection.

Landuse zones share these ring arrays.

### 4.5 Other sections

**Junctions** (24·`junction_count`, array-of-structs):
`u32 junction_id` @0, `u32 str_name` @4, `f32 x_m` @8, `f32 y_m` @12, `f32 z_m` @16,
`u8 control` @20 (`0` none, `1` priority, `2` signal, `3` stop, `4` yield, `5` roundabout),
`u8 reserved` @21, `u16 lane_count` @22.

**Signals** (28·`signal_count`, array-of-structs):
`u32 signal_id` @0, `u32 junction_id` @4, `u32 lane_id` @8 (the controlled lane),
`f32 x_m` @12, `f32 y_m` @16, `f32 z_m` @20, `u8 kind` @24 (`0` vehicle, `1` pedestrian, `2` bicycle,
`3` transit), `u8 reserved` @25, `u16 group` @26 (signal group / phase group id).

**Sites** (32·`site_count`, array-of-structs):
`u32 site_id` @0, `u32 node_id` @4 (`0xFFFFFFFF` if unassigned), `f32 x_m` @8, `f32 y_m` @12,
`f32 z_m` @16 (ground), `f32 antenna_height_m` @20, `f32 antenna_gain_dbi` @24, `u8 kind` @28
(`0` rsu, `1` cell, `2` other), `u8 reserved` @29, `u16 reserved16` @30.

**Crossings** (28·`crossing_count`, array-of-structs):
`u32 crossing_id` @0, `u32 junction_id` @4, `f32 x1_m` @8, `f32 y1_m` @12, `f32 x2_m` @16,
`f32 y2_m` @20, `f32 width_m` @24.

**Landuse** (16·`landuse_count`, array-of-structs):
`u32 landuse_id` @0, `u32 ring_off` @4, `u32 ring_count` @8, `u8 class` @12 (`0` urban, `1` suburban,
`2` rural, `3` highway, `4` water, `5` park, `6` industrial), `u8 reserved` @13, `u16 reserved16` @14.

**Provenance** (`off_provenance_json`, `provenance_json_bytes`): a UTF-8 JSON object, the serialised
`WorldProvenance` of 03-interfaces §2 — `{source, bbox, imported_at, tool_versions, transformations,
licence, dropped}`. It is JSON because it is read once, by a human or a manifest writer.

**Symbol table** (`off_strings`): a `StrTable` (§2.5) scoped to the world file; its ids are independent of
the connection's symbol table.

### 4.6 JSON form

`GET /world/{hash}.json` returns the same content as JSON, for debugging, tests and third-party tools. It
is a direct transcription: each binary section becomes an array of objects, and the parallel `f32` point
arrays become flat number arrays.

```json
{
  "schema": "vwp-world/1",
  "content_hash": "d172872b…e988",
  "origin": {"lat_deg": 52.5163, "lon_deg": 13.3777, "alt_m": 34.0},
  "bbox": {"min_x_m": -500, "min_y_m": -500, "max_x_m": 500, "max_y_m": 500,
           "min_z_m": -2.0, "max_z_m": 61.5},
  "lanes": [
    {"lane_id": 42, "edge_id": 7, "junction_id": null, "name": "Unter den Linden",
     "width_m": 3.25, "speed_limit_mps": 13.89, "lane_type": "drive",
     "index_in_edge": 0, "allowed_classes": ["car","truck","bus","moto","emergency"],
     "centreline": [-120.0, -3.2, 0.15,  -60.0, -3.2, 0.14,  12.345, -3.21, 0.15]}
  ],
  "buildings": [
    {"building_id": 3, "height_m": 21.5, "base_z_m": 0.0, "levels": 6,
     "material": "concrete", "lod_hint": "box",
     "ring": [10.0, 20.0,  40.0, 20.0,  40.0, 55.0,  10.0, 55.0]}
  ],
  "junctions": [{"junction_id": 1, "x_m": 0, "y_m": 0, "z_m": 0.1, "control": "signal", "lane_count": 12}],
  "signals":   [{"signal_id": 7, "junction_id": 1, "lane_id": 42, "x_m": -8.0, "y_m": -6.0, "z_m": 5.2,
                 "kind": "vehicle", "group": 2}],
  "sites":     [{"site_id": 0, "node_id": 2, "x_m": 12.0, "y_m": -8.0, "z_m": 0.0,
                 "antenna_height_m": 6.0, "antenna_gain_dbi": 5.0, "kind": "rsu"}],
  "crossings": [], "landuse": [],
  "provenance": {"source": "osm", "bbox": [13.3700,52.5120,13.3850,52.5210],
                 "imported_at": "2026-09-14T11:02:31Z",
                 "tool_versions": {"v2xw-world": "0.4.0", "osm2streets-rs": "0.3.1"},
                 "transformations": ["local-tangent-plane", "simplify:0.25m", "height-default:3m/level"],
                 "dropped": {"building_holes": 41},
                 "licence": "ODbL-1.0"}
}
```

The JSON form MUST hash to the same `content_hash` as the binary form (the hash is of the **binary** body;
the JSON carries it for cross-checking, it is not a hash of the JSON).

---

## 5. Visibility and the `NODE-only` profile

### 5.1 The three tags

Every channel, every `Keyframe`/`Delta` column and every telemetry field carries one of:

| Tag | Meaning | Available in `node` profile |
|---|---|---|
| **GT** | ground truth — knowable only by the simulator, never by a real receiver | **no** |
| **NODE** | what a real node computes or observes from what it received | yes |
| **PUBLIC** | knowable by anyone standing in the street, or published by the protocol | yes |
| MIXED | a record with both GT and NODE columns | yes, with GT columns blanked |
| DERIVED | computed by a metric provider over other channels; inherits the strictest tag of its inputs | yes if its inputs are |
| META | manifest / protocol scaffolding | yes |

### 5.2 The exact GT set

A `node`-profile server MUST NOT emit any of the following. This list is exhaustive for v1; a conformance
test enumerates it.

**Channels (whole channel withheld)**
`gt.kinematics` (1), `gt.attack.action` (2), `gt.spawn` (3), `gt.despawn` (4).

**`Hello`**
- `nodes.flags` bit1 `IS_ATTACKER` MUST be 0.
- GT channels MUST be absent from the channel table (not merely `enabled = 0`).

**`Keyframe` / `Delta`**

| Field | Blanked to |
|---|---|
| `lane_id` (keyframe column 4; spawn column 6) | `0xFFFFFFFF` |
| the whole delta lane block | absent; `lane_count = 0`, `MFLAG_LANE_CHANGED` clear |
| `accel_cq` (keyframe column 8; moved column 7) | `0` |
| `state` bit 0 `ST_ATTACKER` | `0` |
| spawn `cause`, despawn `cause` | `0xFFFF` |
| rows for actors without `ST_EQUIPPED` | the slot is left empty (`actor_id = 0xFFFFFFFF`) |

The last rule is the principled one: an unequipped pedestrian transmits nothing, so no node-visible data
source can know it exists. Equipped actors keep their pose — the viewport needs a scene to draw, and an
equipped actor's position is exactly what its own broadcasts assert. **`DECISION`: the `node` profile
strips labels, identity linkage and truth of a claim; it does not strip the geometry of transmitting
actors. The purpose is blind evaluation of detection and response, not an occlusion simulator.**

**`Telemetry`**

| Field | Blanked to |
|---|---|
| `clock_offset_ns` | `0` |
| `pos_error_m` | `NaN` |
| `node_state` value `6` (compromised) | reported as `2` (active) |

**Event payloads**

| Channel | Field | Blanked to |
|---|---|---|
| `phy.rx` (11) | `tx_node` | `0xFFFFFFFF` |
| `phy.rx` (11) | `distance_m` | `NaN` |
| `phy.rx` (11) | `los_class` | `0xFF` |
| `node.neighbor` (16) | `peer_actor_id` | `0xFFFFFFFF` |
| `det.observation` (30) | `subject_actor_id` | `0xFFFFFFFF` |
| `app.warning` (40) | `truth`, `subject_actor_id` | `0`, `0xFFFFFFFF` |
| `ma.*` (31/32/33) | `subject_actor_id` | `0xFFFFFFFF` |
| `proto.revocation` (22) | records with `stage ≤ 4` | withheld entirely |
| `gt.attack.action` (2) | whole record | withheld |

**`MetricSample`**
Samples whose `visibility` is `0` (GT) are not emitted. This removes `ttc_min`, `pet`, `drac`,
`flow`/`density`/`mean_speed` (GT traffic), `det_precision`/`det_recall`, `time_to_detect`,
`false_accusations`, `linkability_rate`, `tracking_duration`, `residual_harm`, and `nar` (which joins
`phy.rx` with `gt.kinematics`).

### 5.3 Profile mechanics

- The profile is chosen at connect: `?profile=node`. It is **immutable for the connection**; no control
  method can change it. To switch, the client reconnects.
- The server sets `HELLO_NODE_ONLY` in `hello_flags`, `Keyframe.profile = 1`, and `FLAG_NODE_ONLY` on
  every canonical frame.
- Blanking happens at the **producer**, before serialisation, not at the socket. A `node`-profile stream is
  therefore also a valid recording of a blinded run.
- Any control method that would reveal GT returns error `-32040 visibility_denied` with
  `data = {"field": "...", "visibility": "GT"}`. This applies to `events.set` on a GT channel,
  `metrics.query` for a GT metric, `inspect.entity` on an `Attacker` entity, and `explain` on a GT subject.
- A run recorded in `full` can be replayed under `profile=node` (the replay reader blanks on the way out).
  A run recorded under `node` can never be replayed as `full` — the data is not there. `Hello.scenario_hash`
  is unaffected either way, so both are traceable to the same scenario.
- Overlays that render GT (attacker markers, GT position vs belief) MUST be absent from the overlay list
  returned by `overlay.set`'s introspection in the `node` profile (09-ui §6: "ground-truth overlays … can
  be locked off for blind evaluation").

---
## 6. JSON-RPC 2.0 control surface

### 6.1 Framing

Text frames on the same socket carry JSON-RPC 2.0 (single objects only; **`DECISION`: batch requests are
not supported — they complicate ordering against the binary stream and every real caller sends one call at
a time**; a server receiving a JSON array MUST reply `-32600`).

- `id` is a string or integer chosen by the client; the server echoes it.
- Requests without `id` are notifications; the server MUST NOT reply.
- The server also *sends* notifications (never requests) — see §6.14.
- Control calls and binary frames are **not** ordered with respect to each other except as stated per
  method (`run.seek` and `run.step` define their own ordering guarantee).

### 6.2 JSON-RPC over HTTP

`POST /rpc` with `Content-Type: application/json` accepts the same methods and returns the same results,
for CLI, notebooks and CI. Methods that are connection-scoped (`view.follow`, `view.camera`,
`overlay.set`) return `-32009 not_supported_on_http`. This is the surface the Python API and the generated
CLI (02-architecture §12) are built on.

### 6.3 Self-description

`rpc.discover` returns an **OpenRPC 1.3.2** document describing every method with its params schema,
result schema, errors, summary and examples. **`DECISION`: OpenRPC, not a bespoke registry — it is the
standard self-description format for JSON-RPC, and the copilot's tool registry (09-ui §6/§8) can be
generated from it mechanically, as can the CLI and the typed Python stubs.**

The document is also served statically at `GET /rpc/schema`. Every schema in this section is a fragment of
it. `rpc.discover` MUST be implemented by every server.

### 6.4 Error codes

| Code | Name | Meaning |
|---|---|---|
| −32700 | `parse_error` | invalid JSON |
| −32600 | `invalid_request` | not a valid JSON-RPC 2.0 request object |
| −32601 | `method_not_found` | |
| −32602 | `invalid_params` | `data` = `[{"path": "/speed", "message": "...", "hint": "..."}]` |
| −32603 | `internal_error` | |
| −32000 | `run_not_found` | no such run id |
| −32001 | `run_already_running` | |
| −32002 | `run_not_running` | pause/step/stop with nothing to act on |
| −32003 | `seek_out_of_range` | `data = {"min_ns":…, "max_ns":…}` |
| −32004 | `scenario_invalid` | `data = {"errors":[{"path","message","hint"}]}` (03-interfaces §13) |
| −32005 | `world_not_found` | unknown world hash or unreadable source |
| −32006 | `unknown_id` | unknown node/link/entity/actor/lane id; `data = {"kind","id"}` |
| −32007 | `unknown_metric` | `data = {"metric": "...", "did_you_mean": [...]}` |
| −32008 | `export_failed` | `data = {"stage": "...", "detail": "..."}` |
| −32009 | `not_supported_here` | replay-only or live-only or HTTP-only restriction; `data = {"why": "..."}` |
| −32010 | `busy` | another long operation holds the run; `data = {"operation": "...", "job_id": "..."}` |
| −32011 | `experiment_not_found` | |
| −32012 | `plugin_drift` | replay refuses a different plug-in hash (02-architecture §8) |
| −32013 | `io_error` | file system / network failure; `data = {"path": "...", "errno": "..."}` |
| −32040 | `visibility_denied` | the `node` profile forbids this; `data = {"field","visibility"}` |
| −32041 | `unauthorized` | |
| −32042 | `rate_limited` | `data = {"retry_after_ms": 500}` |
| −32050 | `unsupported_version` | `data = {"server": "1.0", "supported_major": [1]}` |

### 6.5 Shared schema definitions

Referenced below as `#/$defs/<name>`; they live in the OpenRPC document's `components.schemas`.

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$defs": {
    "SimTimeNs":  {"type": "integer", "minimum": 0, "maximum": 18446744073709551615,
                   "description": "nanoseconds since t0"},
    "RunId":      {"type": "string", "format": "uuid"},
    "NodeId":     {"type": "integer", "minimum": 0, "maximum": 4294967294},
    "ActorId":    {"type": "integer", "minimum": 0, "maximum": 4294967294},
    "LaneId":     {"type": "integer", "minimum": 0, "maximum": 4294967294},
    "Sha256Hex":  {"type": "string", "pattern": "^[0-9a-f]{64}$"},
    "Visibility": {"enum": ["GT", "NODE", "PUBLIC", "MIXED", "DERIVED", "META"]},
    "RunState":   {"enum": ["idle", "loading", "running", "paused", "seeking", "finished", "error"]},
    "ChannelName":{"type": "string",
                   "pattern": "^[a-z][a-z0-9]*(\\.[a-z][a-z0-9_]*)+$"},
    "CameraMode": {"enum": ["map", "chase", "dashboard", "free", "rsu", "jump"]},
    "Vec3":       {"type": "object", "additionalProperties": false,
                   "required": ["x", "y", "z"],
                   "properties": {"x": {"type": "number"}, "y": {"type": "number"},
                                  "z": {"type": "number"}}},
    "ValueRef":   {"type": "object", "additionalProperties": false,
                   "required": ["kind"],
                   "properties": {
                     "kind":    {"enum": ["metric","node_field","actor_field","event","link","entity","channel","world","overlay"]},
                     "id":      {"type": "string", "description": "metric name, field path, channel name, …"},
                     "node":    {"$ref": "#/$defs/NodeId"},
                     "actor":   {"$ref": "#/$defs/ActorId"},
                     "t_ns":    {"$ref": "#/$defs/SimTimeNs"},
                     "prov_id": {"type": "integer", "minimum": 1}}},
    "Provenance": {"type": "object", "additionalProperties": false,
                   "required": ["prov_id","model_id","model_version","param_set_id"],
                   "properties": {
                     "prov_id":       {"type": "integer"},
                     "model_id":      {"type": "string"},
                     "model_version": {"type": "string"},
                     "param_set_id":  {"type": "string"},
                     "family":        {"type": "string"},
                     "card_url":      {"type": "string", "format": "uri-reference"},
                     "parameters":    {"type": "object", "additionalProperties": true},
                     "sources":       {"type": "array", "items": {"type": "object"}},
                     "equations":     {"type": "array", "items": {"type": "object"}},
                     "assumptions":   {"type": "array", "items": {"type": "string"}},
                     "validation":    {"type": "object"}}},
    "ValidationError": {"type": "object", "additionalProperties": false,
                   "required": ["path","message"],
                   "properties": {"path": {"type": "string"},
                                  "message": {"type": "string"},
                                  "hint": {"type": "string"},
                                  "severity": {"enum": ["error","warning"]}}},
    "WorldImportResult": {"type": "object", "additionalProperties": false,
                   "required": ["world_hash","bbox_m","lanes","buildings","junctions","bytes","cached"],
                   "properties": {
                     "world_hash": {"$ref": "#/$defs/Sha256Hex"},
                     "url":        {"type": "string"},
                     "bbox_m":     {"type": "object",
                                    "properties": {"min_x": {"type": "number"}, "min_y": {"type": "number"},
                                                   "max_x": {"type": "number"}, "max_y": {"type": "number"}}},
                     "origin":     {"type": "object",
                                    "properties": {"lat_deg": {"type": "number"},
                                                   "lon_deg": {"type": "number"},
                                                   "alt_m": {"type": "number"}}},
                     "lanes":      {"type": "integer"},
                     "buildings":  {"type": "integer"},
                     "junctions":  {"type": "integer"},
                     "signals":    {"type": "integer"},
                     "bytes":      {"type": "integer"},
                     "cached":     {"type": "boolean"},
                     "licence":    {"type": "string"},
                     "warnings":   {"type": "array", "items": {"$ref": "#/$defs/ValidationError"}},
                     "provenance": {"type": "object"}}},
    "Job":        {"type": "object", "additionalProperties": false,
                   "required": ["job_id","state"],
                   "properties": {"job_id": {"type": "string"},
                                  "state": {"enum": ["queued","running","done","failed","cancelled"]},
                                  "progress": {"type": "number", "minimum": 0, "maximum": 1},
                                  "message": {"type": "string"},
                                  "outputs": {"type": "array", "items": {"type": "string"}}}}
  }
}
```

---

### 6.6 Run control

#### `run.start`

Loads a scenario (or the already-set one) and begins producing the stream.

```json
{"params": {"type":"object","additionalProperties":false,
  "properties": {
    "scenario":  {"oneOf":[{"type":"string","description":"path or preset id"},
                           {"type":"object","description":"inline scenario document (schema v2xw/scenario/1)"}]},
    "seed":      {"type":"integer","minimum":0,"description":"overrides scenario.seed"},
    "speed":     {"type":"number","minimum":0,"maximum":100,"default":1,
                  "description":"0 = as fast as possible"},
    "paused":    {"type":"boolean","default":false,"description":"start paused at t=0"},
    "record":    {"type":"boolean","default":true,"description":"write the MCAP recording"},
    "record_path": {"type":"string"},
    "label":     {"type":"string","maxLength":120}}},
 "result": {"type":"object","additionalProperties":false,
  "required":["run_id","state","world_hash","scenario_hash"],
  "properties": {
    "run_id":        {"$ref":"#/$defs/RunId"},
    "state":         {"$ref":"#/$defs/RunState"},
    "world_hash":    {"$ref":"#/$defs/Sha256Hex"},
    "scenario_hash": {"$ref":"#/$defs/Sha256Hex"},
    "recording_path":{"type":"string"},
    "t_end_ns":      {"$ref":"#/$defs/SimTimeNs"}}},
 "errors": [-32001, -32004, -32005, -32013]}
```

The server sends a fresh `Hello` on this connection before the first `Keyframe` of the new run.

#### `run.pause`

```json
{"params": {"type":"object","additionalProperties":false,"properties":{}},
 "result": {"type":"object","required":["state","t_ns"],"additionalProperties":false,
            "properties":{"state":{"$ref":"#/$defs/RunState"},"t_ns":{"$ref":"#/$defs/SimTimeNs"}}},
 "errors": [-32002]}
```

Pausing is at the next mobility-step boundary; the reply's `t_ns` is that boundary. The server MUST have
sent every frame up to and including `t_ns` before replying.

#### `run.resume`

Identical params/result to `run.pause`. Errors: `-32002` if not paused.

#### `run.step`

```json
{"params": {"type":"object","additionalProperties":false,
  "properties": {"unit":  {"enum":["step","event","keyframe","second"],"default":"step"},
                 "count": {"type":"integer","minimum":1,"maximum":100000,"default":1}}},
 "result": {"type":"object","required":["state","t_ns","stepped"],"additionalProperties":false,
  "properties": {"state":{"$ref":"#/$defs/RunState"},"t_ns":{"$ref":"#/$defs/SimTimeNs"},
                 "stepped":{"type":"integer"},
                 "last_event":{"type":"object","description":"present when unit=event",
                   "properties":{"channel":{"$ref":"#/$defs/ChannelName"},
                                 "priority":{"type":"integer"},"seq":{"type":"integer"}}}}},
 "errors": [-32002]}
```

`unit: "event"` steps the DES event heap one event at a time using the `(time, priority, seq)` order of
02-architecture §5.1 — that is what the "step one event" control of 09-ui §6 means. The server MUST flush
all frames produced by the step before replying.

#### `run.seek`

```json
{"params": {"type":"object","additionalProperties":false,
  "oneOf": [{"required":["t_ns"]}, {"required":["fraction"]}, {"required":["event"]}],
  "properties": {
    "t_ns":     {"$ref":"#/$defs/SimTimeNs"},
    "fraction": {"type":"number","minimum":0,"maximum":1},
    "event":    {"type":"object","additionalProperties":false,
                 "required":["channel"],
                 "properties":{"channel":{"$ref":"#/$defs/ChannelName"},
                               "direction":{"enum":["next","prev"],"default":"next"},
                               "node":{"$ref":"#/$defs/NodeId"}}},
    "pause_after": {"type":"boolean","default":true}}},
 "result": {"type":"object","required":["t_ns","keyframe_seq","deltas_applied","elapsed_ms"],
  "additionalProperties":false,
  "properties": {"t_ns":{"$ref":"#/$defs/SimTimeNs"},
                 "keyframe_seq":{"type":"integer"},
                 "deltas_applied":{"type":"integer"},
                 "elapsed_ms":{"type":"number"},
                 "state":{"$ref":"#/$defs/RunState"}}},
 "errors": [-32003, -32009]}
```

Ordering guarantee: the server MUST send the `Keyframe` (with `FLAG_SEEK_RESULT | FLAG_RESYNC`) and all
deltas up to `t_ns` **before** the JSON-RPC reply. A client can therefore treat the reply as "the scene is
now at `t_ns`". Live runs support `run.seek` only backwards into recorded time and only when
`HELLO_SEEKABLE` is set; seeking a live run pauses it.

#### `run.speed`

```json
{"params": {"type":"object","additionalProperties":false,"required":["speed"],
  "properties": {"speed": {"type":"number","minimum":0,"maximum":100,
                           "description":"multiple of real time; 0 = unthrottled"},
                 "sync":  {"enum":["free","client"],"default":"free",
                           "description":"client = pace the producer to this connection (lossless demos)"}}},
 "result": {"type":"object","required":["speed","sync"],"additionalProperties":false,
  "properties": {"speed":{"type":"number"},"sync":{"enum":["free","client"]}}},
 "errors": [-32002]}
```

#### `run.stop`

```json
{"params": {"type":"object","additionalProperties":false,
  "properties": {"finalize_exports": {"type":"boolean","default":true}}},
 "result": {"type":"object","required":["state","t_ns"],"additionalProperties":false,
  "properties": {"state":{"$ref":"#/$defs/RunState"},"t_ns":{"$ref":"#/$defs/SimTimeNs"},
                 "recording_path":{"type":"string"},
                 "digest":{"$ref":"#/$defs/Sha256Hex"},
                 "files":{"type":"array","items":{"type":"object",
                   "required":["path","sha256","bytes"],
                   "properties":{"path":{"type":"string"},"sha256":{"$ref":"#/$defs/Sha256Hex"},
                                 "bytes":{"type":"integer"}}}}}},
 "errors": [-32002, -32008]}
```

The server then sends `Bye{reason = 1}`.

#### `run.status`

```json
{"params": {"type":"object","additionalProperties":false,
            "properties":{"run_id":{"$ref":"#/$defs/RunId"}}},
 "result": {"type":"object",
  "required":["run_id","state","t_ns","t_end_ns","speed","profile","live"],
  "additionalProperties":false,
  "properties": {
    "run_id":{"$ref":"#/$defs/RunId"},
    "state":{"$ref":"#/$defs/RunState"},
    "t_ns":{"$ref":"#/$defs/SimTimeNs"},
    "t_end_ns":{"$ref":"#/$defs/SimTimeNs"},
    "speed":{"type":"number"},
    "sync":{"enum":["free","client"]},
    "profile":{"enum":["full","node"]},
    "live":{"type":"boolean"},
    "wall_elapsed_s":{"type":"number"},
    "realtime_factor":{"type":"number","description":"simulated seconds per wall second"},
    "actors":{"type":"integer"},
    "nodes":{"type":"integer"},
    "events_per_s":{"type":"number"},
    "seq":{"type":"integer","description":"next canonical seq"},
    "dropped":{"type":"object","properties":{"delta":{"type":"integer"},"event":{"type":"integer"},
               "telemetry":{"type":"integer"},"metric":{"type":"integer"}}},
    "manifest":{"type":"object","description":"the run manifest (02-architecture §6.5)"},
    "warnings":{"type":"array","items":{"$ref":"#/$defs/ValidationError"}}}},
 "errors": [-32000]}
```

---

### 6.7 View control (connection-scoped)

#### `view.follow`

```json
{"params": {"type":"object","additionalProperties":false,
  "properties": {
    "node":   {"$ref":"#/$defs/NodeId"},
    "actor":  {"$ref":"#/$defs/ActorId"},
    "camera": {"$ref":"#/$defs/CameraMode"},
    "clear":  {"type":"boolean","default":false},
    "telemetry": {"type":"boolean","default":true,
                  "description":"subscribe this node to Telemetry frames"},
    "radius_m": {"type":"number","minimum":0,"maximum":5000,"default":0,
                 "description":"also subscribe nodes within this radius"}}},
 "result": {"type":"object","required":["following","subscribed_nodes"],"additionalProperties":false,
  "properties": {"following":{"type":["integer","null"]},
                 "camera":{"$ref":"#/$defs/CameraMode"},
                 "subscribed_nodes":{"type":"array","items":{"$ref":"#/$defs/NodeId"}}}},
 "errors": [-32006, -32009]}
```

`view.follow` is the subscription control for `Telemetry`: the server sends telemetry only for subscribed
nodes, which is what keeps the telemetry frame small.

#### `view.camera`

```json
{"params": {"type":"object","additionalProperties":false,"required":["mode"],
  "properties": {
    "mode":     {"$ref":"#/$defs/CameraMode"},
    "target":   {"$ref":"#/$defs/Vec3"},
    "position": {"$ref":"#/$defs/Vec3"},
    "fov_deg":  {"type":"number","minimum":1,"maximum":150},
    "projection": {"enum":["perspective","orthographic"],"default":"perspective"},
    "extent_m": {"type":"number","minimum":1,"description":"map mode: ground extent to cover"},
    "node":     {"$ref":"#/$defs/NodeId","description":"for mode rsu|jump"},
    "animate_ms": {"type":"integer","minimum":0,"maximum":10000,"default":800}}},
 "result": {"type":"object","required":["mode","position","target","fov_deg","projection"],
  "additionalProperties":false,
  "properties": {"mode":{"$ref":"#/$defs/CameraMode"},
                 "position":{"$ref":"#/$defs/Vec3"},"target":{"$ref":"#/$defs/Vec3"},
                 "fov_deg":{"type":"number"},"projection":{"enum":["perspective","orthographic"]}}},
 "errors": [-32006, -32602, -32009]}
```

The camera lives in the client; the method exists so a copilot, a notebook or a CI screenshot job can drive
it (09-ui §8). The server forwards it to the owning client as a `view.changed` notification and echoes the
resolved state.

#### `overlay.set`

```json
{"params": {"type":"object","additionalProperties":false,
  "properties": {
    "overlays": {"type":"object","additionalProperties":{"type":"boolean"},
      "propertyNames": {"enum": ["tx_pulses","links","cbr_heatmap","coverage","attackers_gt",
                                 "revoked","reported","detections","backend_flows","focus_region",
                                 "lane_markings","buildings","labels","trajectories_gt",
                                 "belief_vs_truth_gt","signal_state","rsu_range","density"]}},
    "opacity":  {"type":"object","additionalProperties":{"type":"number","minimum":0,"maximum":1}},
    "list":     {"type":"boolean","default":false,"description":"return the catalogue and do nothing else"}}},
 "result": {"type":"object","required":["overlays"],"additionalProperties":false,
  "properties": {"overlays":{"type":"object","additionalProperties":{"type":"boolean"}},
                 "catalogue":{"type":"array","items":{"type":"object",
                   "required":["name","visibility","available"],
                   "properties":{"name":{"type":"string"},
                                 "visibility":{"$ref":"#/$defs/Visibility"},
                                 "available":{"type":"boolean"},
                                 "description":{"type":"string"},
                                 "needs_channels":{"type":"array","items":{"$ref":"#/$defs/ChannelName"}}}}}}},
 "errors": [-32040, -32602]}
```

Overlays whose name ends in `_gt` have `visibility: "GT"`; in the `node` profile they are reported with
`available: false` and enabling them returns `-32040`.

---

### 6.8 Inspection

#### `inspect.node`

```json
{"params": {"type":"object","additionalProperties":false,"required":["node"],
  "properties": {"node": {"$ref":"#/$defs/NodeId"},
                 "t_ns": {"$ref":"#/$defs/SimTimeNs","description":"default: now"},
                 "include": {"type":"array","uniqueItems":true,
                   "items":{"enum":["telemetry","stores","queues","neighbors","certs","crl",
                                    "gnss","clock","apps","detectors","provenance"]},
                   "default":["telemetry","queues","neighbors"]},
                 "limit": {"type":"integer","minimum":1,"maximum":1000,"default":50,
                           "description":"row cap for list-valued sections"}}},
 "result": {"type":"object","required":["node","t_ns","kind","profile_id"],
  "additionalProperties":true,
  "properties": {
    "node":{"$ref":"#/$defs/NodeId"}, "t_ns":{"$ref":"#/$defs/SimTimeNs"},
    "kind":{"enum":["obu","vru-device","rsu","base-station","router","backend-entity","other"]},
    "label":{"type":"string"}, "profile_id":{"type":"string"},
    "actor":{"$ref":"#/$defs/ActorId"},
    "telemetry":{"type":"object","description":"the §3.5.2 record as named JSON fields with units"},
    "queues":{"type":"object","additionalProperties":{"type":"object",
      "properties":{"depth":{"type":"integer"},"p50":{"type":"number"},"p95":{"type":"number"},
                    "policy":{"type":"string"},"drops":{"type":"object"}}}},
    "stores":{"type":"object","properties":{
      "cert_store":{"type":"object"},"peer_cache":{"type":"object"},"crl_store":{"type":"object"},
      "trust_store":{"type":"object"},"neighbor_table":{"type":"object"},
      "evidence_buffer":{"type":"object"},"report_outbox":{"type":"object"}}},
    "neighbors":{"type":"array","items":{"type":"object",
      "required":["digest","verify_state","last_seen_ns"],
      "properties":{"digest":{"type":"string","pattern":"^[0-9a-f]{16}$"},
                    "verify_state":{"enum":["unverified","verified","failed","revoked"]},
                    "last_seen_ns":{"$ref":"#/$defs/SimTimeNs"},
                    "distance_m":{"type":"number"},"relevance":{"type":"number"},
                    "messages":{"type":"integer"}}}},
    "certs":{"type":"array","items":{"type":"object"}},
    "crl":{"type":"object"}, "gnss":{"type":"object"}, "clock":{"type":"object"},
    "apps":{"type":"array","items":{"type":"object"}},
    "detectors":{"type":"array","items":{"type":"object"}},
    "provenance":{"type":"array","items":{"$ref":"#/$defs/Provenance"}}}},
 "errors": [-32006, -32040]}
```

This is the HUD and inspector payload of 09-ui §5, as JSON, on demand.

#### `inspect.link`

```json
{"params": {"type":"object","additionalProperties":false,
  "oneOf": [{"required":["tx","rx"]}, {"required":["link"]}],
  "properties": {"tx": {"$ref":"#/$defs/NodeId"}, "rx": {"$ref":"#/$defs/NodeId"},
                 "link": {"type":"string","description":"backend/backhaul link id"},
                 "t_ns": {"$ref":"#/$defs/SimTimeNs"},
                 "window_ns": {"$ref":"#/$defs/SimTimeNs","default":1000000000}}},
 "result": {"type":"object","required":["kind","t_ns"],"additionalProperties":true,
  "properties": {
    "kind":{"enum":["radio","backhaul","uu","backend-net"]},
    "t_ns":{"$ref":"#/$defs/SimTimeNs"},
    "distance_m":{"type":"number","description":"GT — absent in the node profile"},
    "los":{"type":"object","properties":{"class":{"enum":["LOS","NLOSb","NLOSv","NLOSt","NLOSbv"]},
           "walls_crossed":{"type":"integer"},"obstructed_len_m":{"type":"number"}}},
    "path_loss_db":{"type":"number"},"shadowing_db":{"type":"number"},"fading_db":{"type":"number"},
    "rx_power_dbm":{"type":"number"},"sinr_db":{"type":"number"},
    "pdr":{"type":"number"},"pir_p95_s":{"type":"number"},
    "frames":{"type":"integer"},"bytes":{"type":"integer"},
    "latency_ms":{"type":"object","properties":{"p50":{"type":"number"},"p95":{"type":"number"}}},
    "bandwidth_mbps":{"type":"number"},
    "provenance":{"type":"array","items":{"$ref":"#/$defs/Provenance"}}}},
 "errors": [-32006, -32040]}
```

#### `inspect.entity`

Backend entities, the MA, protocol flow instances, and any plug-in exposing a `StateView`.

```json
{"params": {"type":"object","additionalProperties":false,"required":["entity"],
  "properties": {"entity": {"type":"string",
                   "description":"role or instance id, e.g. \"ra\", \"pca\", \"ma\", \"crlg\", \"flow:scms.topup#412\""},
                 "t_ns": {"$ref":"#/$defs/SimTimeNs"},
                 "limit": {"type":"integer","minimum":1,"maximum":1000,"default":50}}},
 "result": {"type":"object","required":["entity","t_ns","role","state"],"additionalProperties":true,
  "properties": {
    "entity":{"type":"string"}, "t_ns":{"$ref":"#/$defs/SimTimeNs"},
    "role":{"type":"string"}, "node":{"$ref":"#/$defs/NodeId"},
    "state":{"type":"object","description":"the entity's StateView (03-interfaces §7)"},
    "queue":{"type":"object","properties":{"depth":{"type":"integer"},"servers":{"type":"integer"},
             "utilisation":{"type":"number"},"batch_window_ns":{"$ref":"#/$defs/SimTimeNs"},
             "next_batch_ns":{"$ref":"#/$defs/SimTimeNs"}}},
    "storage_bytes":{"type":"integer"},
    "open_cases":{"type":"integer"}, "decisions":{"type":"integer"},
    "flows":{"type":"array","items":{"type":"object"}},
    "provenance":{"type":"array","items":{"$ref":"#/$defs/Provenance"}}}},
 "errors": [-32006, -32040]}
```

---

### 6.9 `explain`

Resolves any value the UI displays back to the model, version, parameters and sources that produced it —
the "why" tab of 09-ui §5, backed by `Ctx::why` (03-interfaces §1.1).

```json
{"params": {"type":"object","additionalProperties":false,"required":["subject"],
  "properties": {"subject": {"$ref":"#/$defs/ValueRef"},
                 "depth":   {"type":"integer","minimum":1,"maximum":5,"default":1,
                             "description":"follow upstream models this many hops"},
                 "format":  {"enum":["json","markdown"],"default":"json"}}},
 "result": {"type":"object","required":["subject","chain"],"additionalProperties":false,
  "properties": {
    "subject": {"$ref":"#/$defs/ValueRef"},
    "value":   {"description":"the value as displayed, if resolvable"},
    "unit":    {"type":"string"},
    "chain":   {"type":"array","minItems":1,"items":{"$ref":"#/$defs/Provenance"}},
    "definition_md": {"type":"string","description":"for metrics: MetricDef.definition_md"},
    "markdown": {"type":"string","description":"present when format=markdown"},
    "caveats": {"type":"array","items":{"type":"string"},
                "description":"tier assumptions, todo-calibrate parameters, focus-region bias"}}},
 "errors": [-32006, -32007, -32040]}
```

---

### 6.10 Scenario

#### `scenario.get`

```json
{"params": {"type":"object","additionalProperties":false,
  "properties": {"path": {"type":"string","description":"JSON Pointer into the scenario; default whole document"},
                 "resolved": {"type":"boolean","default":true,
                              "description":"true = after base-overlay merge and defaults"},
                 "with_schema": {"type":"boolean","default":false}}},
 "result": {"type":"object","required":["scenario","hash"],"additionalProperties":false,
  "properties": {"scenario":{"type":["object","array","string","number","boolean","null"]},
                 "hash":{"$ref":"#/$defs/Sha256Hex"},
                 "schema":{"type":"object","description":"JSON Schema scenario-1.json, when requested"}}},
 "errors": [-32602]}
```

#### `scenario.set`

```json
{"params": {"type":"object","additionalProperties":false,
  "oneOf": [{"required":["scenario"]}, {"required":["patch"]}],
  "properties": {
    "scenario": {"type":"object","description":"a whole scenario document"},
    "patch":    {"type":"array","description":"RFC 6902 JSON Patch",
                 "items":{"type":"object","required":["op","path"],
                   "properties":{"op":{"enum":["add","remove","replace","move","copy","test"]},
                                 "path":{"type":"string"},"from":{"type":"string"},"value":{}}}},
    "validate": {"type":"boolean","default":true},
    "apply_live": {"type":"boolean","default":false,
                   "description":"apply to the running run where the field allows it (param.change events)"}}},
 "result": {"type":"object","required":["hash","valid"],"additionalProperties":false,
  "properties": {"hash":{"$ref":"#/$defs/Sha256Hex"},"valid":{"type":"boolean"},
                 "errors":{"type":"array","items":{"$ref":"#/$defs/ValidationError"}},
                 "applied_live":{"type":"array","items":{"type":"string"}},
                 "requires_restart":{"type":"array","items":{"type":"string"}}}},
 "errors": [-32004, -32602]}
```

#### `scenario.validate`

```json
{"params": {"type":"object","additionalProperties":false,
  "properties": {"scenario": {"type":"object","description":"default: the current scenario"},
                 "strict": {"type":"boolean","default":false,
                            "description":"treat warnings (unvalidated models at high tier, R2) as errors"}}},
 "result": {"type":"object","required":["valid","errors","warnings"],"additionalProperties":false,
  "properties": {"valid":{"type":"boolean"},
                 "errors":{"type":"array","items":{"$ref":"#/$defs/ValidationError"}},
                 "warnings":{"type":"array","items":{"$ref":"#/$defs/ValidationError"}},
                 "resolved_tiers":{"type":"object","additionalProperties":{"enum":["abstract","medium","high"]}},
                 "estimated_cost":{"type":"object",
                   "properties":{"actors":{"type":"integer"},"nodes":{"type":"integer"},
                                 "realtime_factor":{"type":"number"},
                                 "recording_mb_per_sim_min":{"type":"number"}}}}},
 "errors": [-32602]}
```

Error paths and hints follow 03-interfaces §13:
`{"path":"/radio/tiers/phy","message":"'high' requires mac 'high' (mac is 'medium')","hint":"set radio.tiers.mac to high, or phy to medium","severity":"error"}`.

#### `scenario.save` / `scenario.load` / `scenario.list`

```json
{"scenario.save": {
  "params": {"type":"object","additionalProperties":false,"required":["path"],
    "properties": {"path":{"type":"string"},
                   "scenario":{"type":"object","description":"default: the current scenario"},
                   "overwrite":{"type":"boolean","default":false},
                   "format":{"enum":["yaml","json"],"default":"yaml"}}},
  "result": {"type":"object","required":["path","hash","bytes"],"additionalProperties":false,
    "properties":{"path":{"type":"string"},"hash":{"$ref":"#/$defs/Sha256Hex"},
                  "bytes":{"type":"integer"}}},
  "errors": [-32013, -32004]},

 "scenario.load": {
  "params": {"type":"object","additionalProperties":false,"required":["path"],
    "properties": {"path":{"type":"string","description":"file path or preset id"},
                   "validate":{"type":"boolean","default":true}}},
  "result": {"type":"object","required":["hash","valid","scenario"],"additionalProperties":false,
    "properties":{"hash":{"$ref":"#/$defs/Sha256Hex"},"valid":{"type":"boolean"},
                  "scenario":{"type":"object"},
                  "errors":{"type":"array","items":{"$ref":"#/$defs/ValidationError"}}}},
  "errors": [-32013, -32004]},

 "scenario.list": {
  "params": {"type":"object","additionalProperties":false,
    "properties": {"kind":{"enum":["presets","saved","runs","all"],"default":"all"},
                   "prefix":{"type":"string"},
                   "limit":{"type":"integer","minimum":1,"maximum":1000,"default":100}}},
  "result": {"type":"object","required":["items"],"additionalProperties":false,
    "properties":{"items":{"type":"array","items":{"type":"object",
      "required":["id","kind"],
      "properties":{"id":{"type":"string"},"kind":{"enum":["preset","saved","run"]},
                    "name":{"type":"string"},"description":{"type":"string"},
                    "tags":{"type":"array","items":{"type":"string"}},
                    "path":{"type":"string"},"hash":{"$ref":"#/$defs/Sha256Hex"},
                    "modified":{"type":"string","format":"date-time"}}}}}},
  "errors": [-32013]}}
```

---
### 6.11 World

#### `world.import_osm`

```json
{"params": {"type":"object","additionalProperties":false,
  "oneOf": [{"required":["bbox"]}, {"required":["file"]}],
  "properties": {
    "bbox":  {"type":"array","minItems":4,"maxItems":4,"items":{"type":"number"},
              "description":"[min_lon, min_lat, max_lon, max_lat] WGS-84"},
    "file":  {"type":"string","description":"path to a .osm / .osm.pbf"},
    "buildings": {"type":"boolean","default":true},
    "terrain":   {"oneOf":[{"type":"boolean"},{"type":"string","description":"DEM file path"}],
                  "default":false},
    "simplify_tolerance_m": {"type":"number","minimum":0,"maximum":5,"default":0.25},
    "default_levels_height_m": {"type":"number","minimum":1,"maximum":10,"default":3.0},
    "lane_inference": {"enum":["osm2streets","sumo-netconvert"],"default":"osm2streets"},
    "cache": {"type":"boolean","default":true}}},
 "result": {"oneOf": [{"$ref":"#/$defs/Job"}, {"$ref":"#/$defs/WorldImportResult"}]},
 "errors": [-32005, -32013, -32010]}
```

**`DECISION`: any method that can exceed 2 s returns a `Job` instead of a final result, and reports
progress with the `job.progress` / `job.done` notifications (§6.14), rather than holding the socket open.**
An import that completes inside 2 s returns the `WorldImportResult` directly; a longer one returns a `Job`
whose `job.done` notification carries the same object in `outputs[0]` and whose `world_hash` can then be
read from a repeat call with identical params (imports are content-addressed, so the repeat is free).

#### `world.generate`

```json
{"params": {"type":"object","additionalProperties":false,"required":["kind"],
  "properties": {
    "kind": {"enum":["grid","manhattan","highway","ring","intersection","custom"]},
    "size_m": {"type":"array","minItems":2,"maxItems":2,"items":{"type":"number","minimum":50}},
    "block_m": {"type":"number","minimum":20,"default":120},
    "lanes_per_direction": {"type":"integer","minimum":1,"maximum":6,"default":2},
    "lane_width_m": {"type":"number","minimum":2.0,"maximum":5.0,"default":3.25},
    "speed_limit_mps": {"type":"number","minimum":1,"default":13.89},
    "buildings": {"type":"object","additionalProperties":false,
      "properties": {"density":{"type":"number","minimum":0,"maximum":1,"default":0.6},
                     "height_m":{"type":"array","minItems":2,"maxItems":2,"items":{"type":"number"},
                                 "default":[6,30]},
                     "setback_m":{"type":"number","default":4}}},
    "signals": {"type":"boolean","default":true},
    "seed": {"type":"integer","minimum":0,"default":0},
    "params": {"type":"object","description":"generator-specific, for kind=custom"}}},
 "result": {"oneOf": [{"$ref":"#/$defs/Job"}, {"$ref":"#/$defs/WorldImportResult"}]},
 "errors": [-32602, -32013]}
```

Generation is deterministic in `seed`, so `world_hash` is reproducible.

---

### 6.12 Events, metrics, export

#### `events.set`

Controls which event channels this connection receives, and at what rate.

```json
{"params": {"type":"object","additionalProperties":false,
  "properties": {
    "subscribe":   {"type":"array","items":{"$ref":"#/$defs/ChannelName"}},
    "unsubscribe": {"type":"array","items":{"$ref":"#/$defs/ChannelName"}},
    "only":        {"type":"array","items":{"$ref":"#/$defs/ChannelName"},
                    "description":"replace the whole subscription set"},
    "filter": {"type":"object","additionalProperties":false,
      "properties": {"nodes":{"type":"array","items":{"$ref":"#/$defs/NodeId"}},
                     "follow_radius_m":{"type":"number","minimum":0,"maximum":5000},
                     "min_severity":{"type":"integer","minimum":0,"maximum":3},
                     "sample_1_in":{"type":"integer","minimum":1,"maximum":10000,"default":1}}},
    "max_events_per_step": {"type":"integer","minimum":0,"maximum":1000000,"default":5000,
                            "description":"0 = unlimited; over the cap the server samples deterministically"},
    "list": {"type":"boolean","default":false}}},
 "result": {"type":"object","required":["subscribed"],"additionalProperties":false,
  "properties": {"subscribed":{"type":"array","items":{"type":"object",
                   "required":["channel","channel_id","visibility"],
                   "properties":{"channel":{"$ref":"#/$defs/ChannelName"},
                                 "channel_id":{"type":"integer"},
                                 "visibility":{"$ref":"#/$defs/Visibility"},
                                 "est_rate_per_s":{"type":"number"}}}},
                 "available":{"type":"array","items":{"type":"object"}}}},
 "errors": [-32040, -32602]}
```

**`DECISION`: no event channel is subscribed by default.** A dense scenario produces millions of
`phy.rx` records per simulated second; sending them unasked would swamp any client. The Studio subscribes
`app.warning`, `det.observation`, `proto.revocation` and `sec.cert` at start-up, and subscribes the
high-rate channels only for the followed node.

#### `metrics.query`

```json
{"params": {"type":"object","additionalProperties":false,
  "properties": {
    "metrics": {"type":"array","items":{"type":"string"},
                "description":"metric names; omit to list the catalogue"},
    "t_from_ns": {"$ref":"#/$defs/SimTimeNs"},
    "t_to_ns":   {"$ref":"#/$defs/SimTimeNs"},
    "bin_ns":    {"$ref":"#/$defs/SimTimeNs","default":1000000000},
    "group_by":  {"type":"array","items":{"enum":["t","node","class","dist_bin","density_bin",
                                                  "region","protocol","rat","tier","run","cause"]}},
    "where":     {"type":"object","additionalProperties":{"type":["string","number","array"]}},
    "runs":      {"type":"array","items":{"$ref":"#/$defs/RunId"}},
    "agg":       {"enum":["sum","mean","p50","p95","p99","ratio","rate","max","min"]},
    "format":    {"enum":["json","arrow"],"default":"json",
                  "description":"arrow = an Arrow IPC stream fetched from the returned URL"},
    "limit":     {"type":"integer","minimum":1,"maximum":1000000,"default":10000}}},
 "result": {"type":"object","required":["columns","rows"],"additionalProperties":false,
  "properties": {
    "columns": {"type":"array","items":{"type":"object",
      "required":["name","type"],
      "properties":{"name":{"type":"string"},
                    "type":{"enum":["int","float","string","time_ns"]},
                    "unit":{"type":"string"},
                    "visibility":{"$ref":"#/$defs/Visibility"}}}},
    "rows":  {"type":"array","items":{"type":"array"}},
    "truncated": {"type":"boolean"},
    "arrow_url": {"type":"string","description":"present when format=arrow"},
    "catalogue": {"type":"array","items":{"type":"object",
      "required":["name","unit","dims","agg","visibility"],
      "properties":{"name":{"type":"string"},"unit":{"type":"string"},
                    "dims":{"type":"array","items":{"type":"string"}},
                    "agg":{"type":"string"},"visibility":{"$ref":"#/$defs/Visibility"},
                    "definition_md":{"type":"string"},
                    "source":{"type":"object"}}}},
    "provenance": {"type":"array","items":{"$ref":"#/$defs/Provenance"}}}},
 "errors": [-32007, -32040, -32602]}
```

#### `metrics.plot`

```json
{"params": {"type":"object","additionalProperties":false,"required":["metric"],
  "properties": {
    "metric": {"type":["string","array"],"items":{"type":"string"}},
    "x":  {"enum":["t","dist_bin","density_bin","node","class","run","config"],"default":"t"},
    "by": {"type":"array","items":{"type":"string"},"description":"series split"},
    "runs": {"type":"array","items":{"$ref":"#/$defs/RunId"}},
    "kind": {"enum":["line","scatter","bar","box","heatmap","cdf"],"default":"line"},
    "preset": {"type":"string","description":"figure preset id from 08-measurement §7 (e.g. \"rq3.t95\")"},
    "ci": {"type":"number","minimum":0,"maximum":1,"description":"confidence band across seeds"},
    "render": {"enum":["figure","svg","png","csv"],"default":"figure"},
    "width_px": {"type":"integer","minimum":100,"maximum":4000,"default":900},
    "height_px": {"type":"integer","minimum":100,"maximum":4000,"default":500}}},
 "result": {"type":"object","required":["figure"],"additionalProperties":false,
  "properties": {"figure": {"type":"object",
                   "description":"Plotly figure JSON {data, layout}; identical in the Studio and in the notebook"},
                 "url":    {"type":"string","description":"present when render is svg|png|csv"},
                 "manifest_hash": {"$ref":"#/$defs/Sha256Hex",
                   "description":"stamped into the figure metadata (09-ui §10)"},
                 "provenance": {"type":"array","items":{"$ref":"#/$defs/Provenance"}}}},
 "errors": [-32007, -32040]}
```

#### `export.dataset`

```json
{"params": {"type":"object","additionalProperties":false,"required":["exporter"],
  "properties": {
    "exporter": {"enum":["ma-dataset","receiver-logs","telemetry","net-trace","backend-log","metrics"]},
    "out_dir":  {"type":"string"},
    "opts":     {"type":"object","additionalProperties":true,
                 "description":"exporter-specific (08-measurement §5), e.g. {\"veremi_json\": true}"},
    "visibility": {"enum":["node","gt","both"],"default":"both",
                   "description":"both writes GT into separate files, never mixed"},
    "t_from_ns": {"$ref":"#/$defs/SimTimeNs"},
    "t_to_ns":   {"$ref":"#/$defs/SimTimeNs"},
    "compress":  {"enum":["none","zstd","gzip"],"default":"zstd"}}},
 "result": {"oneOf": [
   {"$ref":"#/$defs/Job"},
   {"type":"object","required":["files","digest"],"additionalProperties":false,
    "properties": {"files":{"type":"array","items":{"type":"object",
                     "required":["path","sha256","bytes","schema","visibility"],
                     "properties":{"path":{"type":"string"},"sha256":{"$ref":"#/$defs/Sha256Hex"},
                                   "bytes":{"type":"integer"},"schema":{"type":"string"},
                                   "visibility":{"$ref":"#/$defs/Visibility"},
                                   "rows":{"type":"integer"}}}},
                   "digest":{"$ref":"#/$defs/Sha256Hex"},
                   "datasheet":{"type":"string"},
                   "leakage_lint":{"type":"object",
                     "properties":{"passed":{"type":"boolean"},
                                   "findings":{"type":"array","items":{"type":"object"}}}}}}]},
 "errors": [-32008, -32013, -32040, -32010]}
```

The leakage linter (08-measurement §5, ported from the legacy `leakage_linter`) MUST run on every export
and its verdict is part of the result.

#### `export.recording`

```json
{"params": {"type":"object","additionalProperties":false,
  "properties": {
    "path":      {"type":"string","description":"output .mcap path; default: the run's recording"},
    "t_from_ns": {"$ref":"#/$defs/SimTimeNs"},
    "t_to_ns":   {"$ref":"#/$defs/SimTimeNs"},
    "channels":  {"type":"array","items":{"$ref":"#/$defs/ChannelName"},
                  "description":"default: all recorded channels"},
    "profile":   {"enum":["full","node"],"default":"full",
                  "description":"node strips every GT channel and GT column (§5)"},
    "compression": {"enum":["zstd","lz4","none"],"default":"zstd"},
    "chunk_mb":  {"type":"number","minimum":0.25,"maximum":64,"default":4}}},
 "result": {"oneOf": [
   {"$ref":"#/$defs/Job"},
   {"type":"object","required":["path","sha256","bytes","messages","channels","t_from_ns","t_to_ns"],
    "additionalProperties":false,
    "properties": {"path":{"type":"string"},"sha256":{"$ref":"#/$defs/Sha256Hex"},
                   "bytes":{"type":"integer"},"messages":{"type":"integer"},
                   "channels":{"type":"array","items":{"type":"string"}},
                   "keyframes":{"type":"integer"},
                   "t_from_ns":{"$ref":"#/$defs/SimTimeNs"},
                   "t_to_ns":{"$ref":"#/$defs/SimTimeNs"},
                   "profile":{"enum":["full","node"]}}}]},
 "errors": [-32008, -32013, -32040]}
```

---

### 6.13 Experiments

#### `experiment.define`

```json
{"params": {"type":"object","additionalProperties":false,"required":["name","sweep"],
  "properties": {
    "name": {"type":"string","maxLength":120},
    "base": {"type":["string","object"],"description":"scenario path or inline document"},
    "sweep": {"type":"object","minProperties":1,
              "additionalProperties":{"type":"array","minItems":1},
              "description":"JSON-Pointer-ish dotted scenario paths → value lists (08-measurement §4)"},
    "seeds": {"type":"object","additionalProperties":false,
              "properties":{"count":{"type":"integer","minimum":1,"maximum":1000,"default":1},
                            "master":{"type":"integer","minimum":0}}},
    "replications_policy": {"type":"object","additionalProperties":false,
      "properties":{"ci":{"type":"number","minimum":0,"exclusiveMaximum":1,"default":0.95},
                    "method":{"enum":["bootstrap","t","none"],"default":"bootstrap"}}},
    "outputs": {"type":"array","items":{"type":"string"},
                "default":["metrics.parquet","comparison.html"]},
    "resources": {"type":"object","additionalProperties":false,
      "properties":{"parallel":{"type":"integer","minimum":1,"maximum":256,"default":4},
                    "tier_overrides_for_large_n":{"type":"object","additionalProperties":{"type":"string"}}}}}},
 "result": {"type":"object","required":["experiment_id","cells","estimated_wall_s"],
  "additionalProperties":false,
  "properties": {"experiment_id":{"type":"string"},
                 "cells":{"type":"integer","description":"cartesian product × seeds"},
                 "cell_keys":{"type":"array","items":{"type":"object"}},
                 "estimated_wall_s":{"type":"number"},
                 "warnings":{"type":"array","items":{"$ref":"#/$defs/ValidationError"}}}},
 "errors": [-32004, -32602]}
```

#### `experiment.run`

```json
{"params": {"type":"object","additionalProperties":false,"required":["experiment_id"],
  "properties": {"experiment_id":{"type":"string"},
                 "resume":{"type":"boolean","default":true,
                           "description":"skip cells whose manifest hash already exists"},
                 "cells":{"type":"array","items":{"type":"integer"},
                          "description":"run only these cell indices"},
                 "runner":{"enum":["local","slurm","k8s"],"default":"local"}}},
 "result": {"allOf": [{"$ref":"#/$defs/Job"},
   {"type":"object","properties":{"experiment_id":{"type":"string"},
                                  "cells_total":{"type":"integer"},
                                  "cells_skipped":{"type":"integer"}}}]},
 "errors": [-32011, -32010, -32013]}
```

#### `experiment.status`

```json
{"params": {"type":"object","additionalProperties":false,
  "properties": {"experiment_id":{"type":"string"},
                 "job_id":{"type":"string"},
                 "include_cells":{"type":"boolean","default":false}}},
 "result": {"type":"object","required":["experiment_id","state","cells_total","cells_done"],
  "additionalProperties":false,
  "properties": {
    "experiment_id":{"type":"string"},
    "state":{"enum":["defined","queued","running","done","failed","cancelled"]},
    "cells_total":{"type":"integer"},"cells_done":{"type":"integer"},
    "cells_failed":{"type":"integer"},
    "progress":{"type":"number","minimum":0,"maximum":1},
    "eta_s":{"type":"number"},
    "outputs":{"type":"array","items":{"type":"string"}},
    "experiment_manifest_hash":{"$ref":"#/$defs/Sha256Hex"},
    "cells":{"type":"array","items":{"type":"object",
      "required":["index","key","state"],
      "properties":{"index":{"type":"integer"},
                    "key":{"type":"object","additionalProperties":true},
                    "state":{"enum":["pending","running","done","failed","skipped"]},
                    "run_id":{"$ref":"#/$defs/RunId"},
                    "manifest_hash":{"$ref":"#/$defs/Sha256Hex"},
                    "recording":{"type":"string"},
                    "error":{"type":"string"}}}}}},
 "errors": [-32011]}
```

#### `rpc.discover`

```json
{"params": {"type":"object","additionalProperties":false,
            "properties": {"method": {"type":"string","description":"limit to one method"}}},
 "result": {"type":"object","required":["openrpc","info","methods"],
            "description":"an OpenRPC 1.3.2 document"},
 "errors": [-32601]}
```

### 6.14 Server → client notifications

The server sends these as JSON-RPC notifications (no `id`, no reply expected).

| Method | Params | When |
|---|---|---|
| `run.state` | `{"state","t_ns","run_id","reason"?}` | any run state transition not caused by the caller |
| `stream.drop` | `{"seq_first","seq_last","dropped":{…},"resync_seq"}` | after a backpressure drop (§1.5) |
| `job.progress` | `{"job_id","progress","message"}` | long operations (import, export, experiment) |
| `job.done` | `{"job_id","state","outputs","error"?}` | long operation finished |
| `view.changed` | `{"mode","position","target","fov_deg","following"}` | another client or the copilot moved the camera |
| `log` | `{"level","target","message","t_ns"?}` | `tracing` events at or above the connection's level |
| `validation` | `{"errors":[…],"warnings":[…]}` | the scenario changed and was re-validated |
| `experiment.progress` | `{"experiment_id","cells_done","cells_total","eta_s"}` | during `experiment.run` |

### 6.15 Method inventory

31 methods plus `rpc.discover` = **32**.

| Group | Methods |
|---|---|
| run (8) | `run.start`, `run.pause`, `run.resume`, `run.step`, `run.seek`, `run.speed`, `run.stop`, `run.status` |
| view (3) | `view.follow`, `view.camera`, `overlay.set` |
| inspect (4) | `inspect.node`, `inspect.link`, `inspect.entity`, `explain` |
| scenario (6) | `scenario.get`, `scenario.set`, `scenario.validate`, `scenario.save`, `scenario.load`, `scenario.list` |
| world (2) | `world.import_osm`, `world.generate` |
| stream (1) | `events.set` |
| metrics (2) | `metrics.query`, `metrics.plot` |
| export (2) | `export.dataset`, `export.recording` |
| experiment (3) | `experiment.define`, `experiment.run`, `experiment.status` |
| meta (1) | `rpc.discover` |

Reserved for later minor versions, listed in 09-ui §8 but out of scope for v1:
`world.import_sumo`, `world.edit`, `metrics.export`, `experiment.compare`, `export.traces`,
`export.telemetry`, `foundry.run`. Adding them is a minor-version change (§8).

---
## 7. Replay

### 7.1 Recording layout

The recording is MCAP (ADR 0008). Mapping:

| MCAP concept | VWP |
|---|---|
| Channel | one per VWP message type plus one per event channel: `vwp/hello`, `vwp/keyframe`, `vwp/delta`, `vwp/telemetry`, `vwp/metric`, `vwp/provenance`, `vwp/event/<channel-name>` |
| Schema | `name = "vwp.v1.<Type>"`, `encoding = "vwp1"`, `data` = the UTF-8 bytes of the relevant section of this document's layout tables (so the file is self-describing) |
| Message `data` | **the complete VWP frame, header included**, stored uncompressed |
| Message `log_time` / `publish_time` | `sim_time_ns` (so MCAP's index is a sim-time index) |
| Message `sequence` | the canonical `seq` |
| Chunk | zstd, target 4 MiB uncompressed; a chunk MUST NOT start in the middle of a GOP's keyframe |
| Metadata record `v2xw.manifest` | the run manifest of 02-architecture §6.5, as JSON |
| Attachment `scenario.yaml`, `world.vwb` | the exact scenario and world bytes |

**`DECISION`: the MCAP message payload is the whole frame including the 24-byte header.** The header is
32 bytes of redundancy per message against a trivially provable byte-identity guarantee and a replay path
that is a memcpy. Per-event payloads inside an `Event` frame are *not* split into one MCAP message each:
one `Event` frame is one MCAP message on `vwp/event/<channel>` when the batch is single-channel, and the
recorder splits mixed batches by channel so that per-channel message indexes work.

**Amendment, 2026-09-19 — this mapping describes the `vwp/…` namespace, and a second
namespace exists alongside it.** As written above, §7.1 places every channel, telemetry,
metric and event included, on a `vwp/…` topic carrying the complete frame. Build decision
D11 item 5 instead routes event, telemetry and metric records through serde to Parquet or
JSONL. Implementing both exposed that neither is the whole story, and that they serve
different consumers rather than competing.

A recording therefore carries **two topic namespaces**, and a single topic never carries
both encodings:

| Namespace | Payload | Encoding | For |
|---|---|---|---|
| `vwp/…` | the complete VWP frame, header included | `vwp1` | the replayable UI stream, where the byte-identity guarantee of §7.2 attaches |
| `record/…` | the serde-encoded `Record` for that channel | `json`, exported to Parquet or JSONL | datasets and analysis, where a self-describing columnar form is worth more than zero copy |

The table above and the `DECISION` note describe the `vwp/…` namespace only. §7.2's
byte-identity guarantee attaches to whatever was actually stored on a `vwp/…` topic; it
makes no claim about `record/…`, which is a lossy-by-design projection into a tabular
schema.

A reader of §7.1 alone would otherwise build a recorder that never writes a Parquet-bound
record, which is why this is stated here rather than only in the build decisions.

### 7.2 Byte-identity of live and replay

> **Guarantee.** For a given run, for every canonical frame, the 24-byte header with
> `flags &= CANONICAL_FLAG_MASK` and the entire body are byte-identical whether the frame was produced by
> the live engine or by the replay reader.

This follows from three properties, each of which is testable:

1. **Determinism of production.** Same scenario + seed + engine build + plug-in set ⇒ identical records
   (02-architecture §6.1). `seq`, `gop_index`, `step_index` and quantised poses are all derived from that
   record stream.
2. **Storage of the produced bytes.** The recorder stores the frame, not a re-encoding of the state.
3. **Flag separation.** Only `TRANSPORT_FLAG_MASK` bits (`COMPRESSED`, `RESYNC`, `CONTINUED`,
   `SEEK_RESULT`) may differ, and they are set by the *sender*, after the frame leaves the producer.
   The recorder MUST write frames with transport bits cleared.

Consequences the client can rely on:

- A client that hashes canonical bodies gets the same digest live and in replay.
- There is no replay-specific code path in the Studio (09-ui §7).
- `Hello` is *not* covered: it carries `resume_seq`, `sim_time_ns` and `hello_flags`, which are
  connection state. The recorded `Hello` is the one produced at run start.

Conformance test `golden_live_vs_replay`: run the Phase 1 scenario live with a recording client, replay the
resulting MCAP, and assert equality frame by frame after masking flags.

### 7.3 The seek algorithm

Given a target `t`:

```
1. footer        := read the last 8 + 20 bytes of the file          → summary_start, summary_offset_start
2. summary       := read [summary_start, footer)                    → channel/schema/statistics/chunk-index records
                    (cached after the first seek; ~1 record per chunk)
3. chunk_index   := the chunk-index records for channel vwp/keyframe
4. kf_chunk      := the last chunk whose message_index for vwp/keyframe has an entry with log_time <= t
5. read+decompress kf_chunk                                          (zstd, ≤ 4 MiB uncompressed)
6. kf            := the last keyframe in kf_chunk with sim_time_ns <= t
7. deltas        := all vwp/delta messages with kf.sim_time_ns < log_time <= t, in log_time order,
                    from kf_chunk and, if the GOP spans a chunk boundary, the next chunk
8. emit kf with FLAG_RESYNC | FLAG_SEEK_RESULT, then the deltas unchanged
9. resume normal production from t
```

Steps 1–3 are done once per file. Step 4 is a binary search over an in-memory array. Because a chunk never
starts in the middle of a keyframe (§7.1) and a GOP is at most `keyframe_period_ns` long, step 7 reads at
most two chunks.

**Optional companion state.** `Telemetry`, `Provenance` and the `sec.cert`/`proto.revocation` channels are
*sparse and stateful*: the value at time `t` is the last one at or before `t`. The reader answers them with
a "latest-at" query using the per-channel message index (the pattern Rerun's primary query cache uses,
cited in ADR 0008), and emits one synthetic `Telemetry` and one `Provenance` frame after the seek keyframe.
Those two frames are **not** part of the byte-identity guarantee and MUST carry `FLAG_SEEK_RESULT`.

### 7.4 The ≤ 100 ms seek budget

| Step | Budget (p95) | Basis |
|---|---|---|
| footer + summary (first seek only) | 5 ms | 2 reads, ≤ 256 KiB |
| chunk-index binary search | < 1 ms | in memory |
| read 1–2 chunks from disk | 15 ms | 2 × 1.5 MiB compressed at ≥ 200 MB/s |
| zstd decompress 1–2 chunks | 25 ms | 2 × 4 MiB at ≥ 400 MB/s |
| locate keyframe, collect ≤ 10 deltas | 3 ms | linear scan of a decompressed chunk |
| serialise + send | 5 ms | ≤ 400 KB, no re-encoding |
| client apply (keyframe + 10 deltas, 10 k actors) | 12 ms | typed-array writes, no parsing |
| **total** | **≤ 66 ms** | 34 ms of slack against the 100 ms target |

CI benchmark `bench_seek` (10-roadmap Phase 1 acceptance): on the `downtown-1km2` 600 s recording,
100 uniformly random seek targets, assert **p95 ≤ 100 ms** and **max ≤ 200 ms**, measured from the
`run.seek` request to the JSON-RPC reply, with the summary already cached. A second variant measures the
cold case (first seek) and asserts ≤ 150 ms.

### 7.5 WASM replay

The same Rust reader compiled to WASM serves the static-hosting mode (09-ui §9). It produces the identical
stream through an in-process channel instead of a socket; `Hello.world_ref.mode` is `1` and the world
arrives as `WorldChunk` frames (§3.9) because there is no engine HTTP server. Everything else is unchanged,
including `seq`, quantisation and the seek algorithm.

---

## 8. Versioning

### 8.1 Version numbers

`MAJOR.MINOR`. `MAJOR` is on the wire in every frame header and in the WebSocket subprotocol token
(`vwp.v1`); `MINOR` is in `Hello.version_minor` only.

- **MAJOR** changes when a reader built for the old version would mis-parse a new frame.
- **MINOR** changes when a reader built for the old version still parses every frame correctly but would
  miss information.

### 8.2 Negotiation

1. Client connects with `Sec-WebSocket-Protocol: vwp.v1` and `?v=1`. A server that does not implement
   major 1 fails the upgrade with HTTP 426 and an `Upgrade` header listing what it does implement.
2. The server sends `Hello` with `version_major` and `version_minor`.
3. The client checks `version_major`. **`DECISION`: the client supports majors N and N−1 (per
   02-architecture §9), so a client built for major 2 accepts `Hello.version_major ∈ {1, 2}` and
   switches parser.** A client that cannot: close with 4406 after logging the mismatch.
4. The client ignores a `version_minor` higher than it knows and keeps parsing; it MUST NOT refuse.
5. A server that receives a request for a major it cannot serve sends `Error{-32050}` + `Bye` and closes
   with 4406, having first sent a `Hello` so the client can read `supported` from the error detail.

### 8.3 What is breaking (requires MAJOR)

- Changing the offset, size, type, unit, scale or meaning of any existing field.
- Removing a field, a section, a message type, a channel id or a JSON-RPC method.
- Reusing an enum value for a different meaning, or renumbering an enum.
- Changing a quantisation scale, an origin rule, or the delta reference rule (§3.2).
- Changing the frame-header size or the `CANONICAL_FLAG_MASK`.
- Changing which fields are GT (§5.2) in a way that *widens* what a `node`-profile stream contains.
- Making a previously optional JSON-RPC param required, or removing a result property.

### 8.4 What is additive (MINOR)

- A new message type id in the reserved range. Readers ignore unknown `msg_type` (§2.1).
- A new event channel id. Readers skip unknown channels by `payload_len` (§3.6.1).
- A new enum value in a range documented as extensible. Readers map unknown values to the enum's
  "other/unknown" member, which every enum in this document has at `0` or `0xFF`.
- New fields **appended into the reserved tail of a record that carries `record_size` on the wire**
  (`Telemetry`, `MetricSample`): the writer raises `record_size`, old readers stride by the new
  `record_size` and read only the prefix they know.
- New trailing **sections**, addressed by an `off_*` field carved out of a prefix's reserved bytes.
  Old readers never look at the reserved bytes, so they never see the section.
- New JSON-RPC methods, new optional params, new result properties, new error codes in
  −32000…−32099. Clients MUST tolerate unknown result properties.
- New string ids, new provenance entries, new overlays, new metrics.

### 8.5 The rule for adding a field

> To add a field in a minor version, it must land in bytes that a v1 reader is already required to ignore:
> a `reserved` field, the tail of a `record_size`-carrying record, or a new section behind a new `off_*`.
> If it cannot, it is a major change. There is no third option, and no field is ever "optional" on the
> wire.

Every prefix in §3 keeps reserved bytes for exactly this purpose: `Keyframe` has 10 (`reserved` +
`reserved64`), `Telemetry` records have 12, `MetricSample` and `Provenance` prefixes have 4, `Error` and
`Bye` have 7, and the world-file directory (§4.2) has 12.

`Hello`'s 256-byte prefix is fully assigned, on purpose. **`DECISION`: `Hello` grows by adding strings and
table rows, never prefix fields.** New per-node, per-class or per-channel information is a new column —
which is a major change — so in practice new `Hello` content in v1.x arrives as a new *string id* whose
meaning is fixed by this document (for example `str_manifest_url`, appended to the symbol table at a
documented index). This is not a limitation in practice: a client that needs structured run metadata calls
`run.status`, whose result is JSON and freely extensible.

### 8.6 Recording compatibility

An MCAP file records `version_major.version_minor` in the manifest metadata and in each schema record. The
replay reader refuses a file with a higher major (error `-32050`) and accepts a higher minor, ignoring what
it does not know. A file with a different plug-in hash set is refused with `-32012` unless
`--allow-plugin-drift` was given, which is then recorded in the manifest (02-architecture §8).

---
## 9. Worked example (unit-test vectors)

Every byte below was generated from the layout tables in §3 and then re-parsed by an independent decoder
written only from those tables: all four frames round-trip, every `off_*` lands where the tables predict,
every section size matches its formula, and the poses reconstruct to the metre values in the table below.
Use it as a golden test in both implementations. Offsets are **frame-relative** (the 24-byte header occupies
`0x00`–`0x17`, the body starts at `0x18`). The `seq` values (0, 10, 11, 12) are illustrative: `Hello`
carries the next canonical seq, and the keyframe and the two deltas here are the 11th, 12th and 13th
canonical frame of the run.

**Scenario of the example**

| Thing | Value |
|---|---|
| run | `0189d4c7-9f3a-7b21-8e44-5c6d7e8f9a0b`, scenario `single-intersection` |
| `scenario_hash` | `SHA-256("vwp-example:scenario:single-intersection")` = `bc46db1c20e90e5621c377aabafa5632de6412a22984f57d80e3f6babdfae184` |
| `world_hash` | `SHA-256("vwp-example:world:single-intersection")` = `d172872bfdd10998babc1497334713546bbbb7dffa1b3b90fddb2d17d641e988` |
| `t0` | `2027-03-04T07:00:00Z` = `1804143600000000000` ns since the Unix epoch |
| world origin | 52.5163 N, 13.3777 E, 34.0 m; bbox `(-500,-500)…(500,500)` m |
| nodes | `0` OBU on actor 0, `1` OBU on actor 1, `2` RSU at (12, −8, 6) |
| classes | `0` car (4.5×1.8×1.5), `1` truck (12.0×2.55×3.6), `2` pedestrian (0.5×0.5×1.75) |
| channels | `node.tx` (10, NODE), `phy.rx` (11, MIXED), `gt.kinematics` (1, GT) |
| keyframe origin | `(-500, -500, 0)` = `floor(bbox_min)` |

Actors in the keyframe at `t = 1.000 s`:

| Slot | Actor | x (m) | y (m) | z (m) | lane | heading | speed (m/s) | accel | class | state | verified nbrs |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 0 | 0 | 12.345 | −3.210 | 0.15 | 42 | 0 rad (east) | 13.89 | +0.5 | car | `0x08` EQUIPPED | 7 |
| 1 | 1 | −20.000 | 0.500 | 0.15 | 43 | π rad (west) | 11.00 | −1.2 | truck | `0x09` EQUIPPED\|ATTACKER | 5 |
| 2 | 2 | 3.000 | 41.250 | 0.10 | none | π/2 (north) | 0.00 | 0.0 | pedestrian | `0x00` | 0 |

Signal `7` is in phase `3` (protected green) with 12.8 s to the next change.

Quantisation worked through, for slot 0:

```
x_mm = round((12.345 − (−500)) × 1000) = round(512345.0) = 512345  → 0x0007D179
y_mm = round((−3.210 − (−500)) × 1000) = round(496790.0) = 496790  → 0x00079496
z_cm = round((0.15 − 0) × 100)         = 15                        → 0x000F
heading_brad = round(0.0 × 65536 / 2π) = 0
speed_cq    = round(13.89 × 128)       = 1778   (13.890625 m/s on decode, error 0.6 mm/s)
accel_cq    = round(0.5 × 64)          = 32     (0.5 m/s² exactly)
```

and for slot 1: `heading_brad = round(π × 65536/2π) = 32768`, `speed_cq = 1408` (11.0 m/s exactly),
`accel_cq = round(−1.2 × 64) = −77` → `0xFFB3` (−1.203125 m/s² on decode).

### 9.1 `Hello` — 796 bytes total (24 header + 772 body)

#### Hello — frame header (24 bytes)

```text
  offset  bytes                                            field
--------  -----------------------------------------------  ----------------------------------------
00000000  56 57 50 31                                      magic = 0x31505756 -> wire bytes "V" "W" "P" "1"
00000004  01 00                                            version = 1 (major)
00000006  01 00                                            msg_type = 0x0001 (Hello)
00000008  04 03 00 00                                      body_len = 772 (uncompressed body bytes)
0000000c  00 00                                            flags = 0x0000
0000000e  00 00                                            reserved = 0
00000010  00 00 00 00 00 00 00 00                          seq = 0
--------
total: 24 bytes
```


#### Hello — body (772 bytes, starts at frame offset 24)

```text
  offset  bytes                                            field
--------  -----------------------------------------------  ----------------------------------------
00000018  01 00                                            version_major = 1
0000001a  00 00                                            version_minor = 0
0000001c  01 00 00 00                                      hello_flags = 0x00000001 (LIVE)
00000020  01 89 d4 c7 9f 3a 7b 21 8e 44 5c 6d 7e 8f 9a 0b  run_id = 0189d4c7-9f3a-7b21-8e44-5c6d7e8f9a0b
00000030  bc 46 db 1c 20 e9 0e 56 21 c3 77 aa ba fa 56 32  scenario_hash = SHA-256
00000040  de 64 12 a2 29 84 f5 7d 80 e3 f6 ba bd fa e1 84  ...
00000050  d1 72 87 2b fd d1 09 98 ba bc 14 97 33 47 13 54  world_hash = SHA-256
00000060  6b bb b7 df fa 1b 3b 90 fd db 2d 17 d6 41 e9 88  ...
00000070  00 60 cb a1 0b 9b 09 19                          t0_wall_ns = 1804143600000000000 (2027-03-04T07:00:00Z)
00000078  00 70 c9 b2 8b 00 00 00                          sim_duration_ns = 600 s
00000080  00 e1 f5 05 00 00 00 00                          mobility_step_ns = 100 ms
00000088  00 ca 9a 3b 00 00 00 00                          keyframe_period_ns = 1 s
00000090  00 ca 9a 3b 00 00 00 00                          telemetry_period_ns = 1 s
00000098  00 ca 9a 3b 00 00 00 00                          metric_period_ns = 1 s
000000a0  00 00 00 00 00 00 00 00                          resume_seq = 0
000000a8  00 00 00 00 00 00 00 00                          sim_time_ns = 0
000000b0  60 76 4f 1e 16 42 4a 40                          origin_lat_deg
000000b8  fe 65 f7 e4 61 c1 2a 40                          origin_lon_deg
000000c0  00 00 00 00 00 00 41 40                          origin_alt_m
000000c8  00 00 00 00 00 40 7f c0                          bbox_min_x_m
000000d0  00 00 00 00 00 40 7f c0                          bbox_min_y_m
000000d8  00 00 00 00 00 40 7f 40                          bbox_max_x_m
000000e0  00 00 00 00 00 40 7f 40                          bbox_max_y_m
000000e8  00 10 00 00                                      actor_capacity = 4096
000000ec  03 00 00 00                                      node_count = 3
000000f0  03 00                                            class_count = 3
000000f2  03 00                                            channel_count = 3
000000f4  00 01 00 00                                      off_nodes = 256
000000f8  60 01 00 00                                      off_classes = 352
000000fc  a8 01 00 00                                      off_channels = 424
00000100  c0 01 00 00                                      off_world_ref = 448
00000104  d0 01 00 00                                      off_strings = 464
00000108  01 00 00 00                                      str_engine_version = 1
0000010c  02 00 00 00                                      str_scenario_name = 2
00000110  03 00 00 00                                      str_run_label = 3
00000114  04 00 00 00                                      str_session_token = 4
00000118  00 00 00 00                                      nodes.node_id[0] = 0
0000011c  01 00 00 00                                      nodes.node_id[1] = 1
00000120  02 00 00 00                                      nodes.node_id[2] = 2
00000124  00 00 00 00                                      nodes.actor_id[0] = 0x00000000
00000128  01 00 00 00                                      nodes.actor_id[1] = 0x00000001
0000012c  ff ff ff ff                                      nodes.actor_id[2] = 0xffffffff
00000130  00 00 00 00                                      nodes.pos_x_m[0] = 0.0
00000134  00 00 00 00                                      nodes.pos_x_m[1] = 0.0
00000138  00 00 40 41                                      nodes.pos_x_m[2] = 12.0
0000013c  00 00 00 00                                      nodes.pos_y_m[0] = 0.0
00000140  00 00 00 00                                      nodes.pos_y_m[1] = 0.0
00000144  00 00 00 c1                                      nodes.pos_y_m[2] = -8.0
00000148  00 00 00 00                                      nodes.pos_z_m[0] = 0.0
0000014c  00 00 00 00                                      nodes.pos_z_m[1] = 0.0
00000150  00 00 c0 40                                      nodes.pos_z_m[2] = 6.0
00000154  06 00 00 00                                      nodes.str_label[0] = 6
00000158  07 00 00 00                                      nodes.str_label[1] = 7
0000015c  08 00 00 00                                      nodes.str_label[2] = 8
00000160  09 00 00 00                                      nodes.str_profile_id[0] = 9
00000164  09 00 00 00                                      nodes.str_profile_id[1] = 9
00000168  0a 00 00 00                                      nodes.str_profile_id[2] = 10
0000016c  01 00                                            nodes.flags[0] = 0x0001 (HAS_HSM)
0000016e  01 00                                            nodes.flags[1] = 0x0001 (HAS_HSM)
00000170  01 00                                            nodes.flags[2] = 0x0001 (HAS_HSM)
00000172  00                                               nodes.kind[0] = 0
00000173  00                                               nodes.kind[1] = 0
00000174  02                                               nodes.kind[2] = 2
00000175  00                                               nodes.class_idx[0] = 0x00
00000176  01                                               nodes.class_idx[1] = 0x01
00000177  ff                                               nodes.class_idx[2] = 0xff
00000178  0b 00 00 00                                      classes.str_name[0] = 11
0000017c  0c 00 00 00                                      classes.str_name[1] = 12
00000180  0d 00 00 00                                      classes.str_name[2] = 13
00000184  00 00 90 40                                      classes.length_m[0] = 4.5
00000188  00 00 40 41                                      classes.length_m[1] = 12.0
0000018c  00 00 00 3f                                      classes.length_m[2] = 0.5
00000190  66 66 e6 3f                                      classes.width_m[0] = 1.8
00000194  33 33 23 40                                      classes.width_m[1] = 2.55
00000198  00 00 00 3f                                      classes.width_m[2] = 0.5
0000019c  00 00 c0 3f                                      classes.height_m[0] = 1.5
000001a0  66 66 66 40                                      classes.height_m[1] = 3.6
000001a4  00 00 e0 3f                                      classes.height_m[2] = 1.75
000001a8  ff f6 82 3b                                      classes.color_rgba[0] = 0x3b82f6ff
000001ac  ff 0b 9e f5                                      classes.color_rgba[1] = 0xf59e0bff
000001b0  ff 81 b9 10                                      classes.color_rgba[2] = 0x10b981ff
000001b4  00 00                                            classes.reserved16[0] = 0
000001b6  00 00                                            classes.reserved16[1] = 0
000001b8  00 00                                            classes.reserved16[2] = 0
000001ba  00                                               classes.category[0] = 0
000001bb  00                                               classes.category[1] = 0
000001bc  01                                               classes.category[2] = 1
000001bd  00                                               classes.reserved8[0] = 0
000001be  00                                               classes.reserved8[1] = 0
000001bf  00                                               classes.reserved8[2] = 0
000001c0  0e 00 00 00                                      channels.str_id[0] = 14
000001c4  0f 00 00 00                                      channels.str_id[1] = 15
000001c8  10 00 00 00                                      channels.str_id[2] = 16
000001cc  0a 00                                            channels.channel_id[0] = 10
000001ce  0b 00                                            channels.channel_id[1] = 11
000001d0  01 00                                            channels.channel_id[2] = 1
000001d2  01                                               channels.visibility[0] = 1
000001d3  03                                               channels.visibility[1] = 3
000001d4  00                                               channels.visibility[2] = 0
000001d5  01                                               channels.enabled[0] = 1
000001d6  01                                               channels.enabled[1] = 1
000001d7  01                                               channels.enabled[2] = 1
000001d8  00                                               world_ref.mode = 0 (HTTP by content hash)
000001d9  00                                               world_ref.format = 0 (vwp-world/1 binary)
000001da  00 00                                            world_ref.reserved = 0
000001dc  d0 a0 16 00                                      world_ref.payload_bytes = 1482960
000001e0  05 00 00 00                                      world_ref.str_url = 5
000001e4  00 00 00 00                                      world_ref.reserved32 = 0
000001e8  11 00 00 00                                      str.n = 17
000001ec  e1 00 00 00                                      str.blob_bytes = 225
000001f0  00 00 00 00                                      str.offsets[0] = 0
000001f4  00 00 00 00                                      str.offsets[1] = 0
000001f8  12 00 00 00                                      str.offsets[2] = 18
000001fc  25 00 00 00                                      str.offsets[3] = 37
00000200  29 00 00 00                                      str.offsets[4] = 41
00000204  33 00 00 00                                      str.offsets[5] = 51
00000208  7e 00 00 00                                      str.offsets[6] = 126
0000020c  86 00 00 00                                      str.offsets[7] = 134
00000210  8e 00 00 00                                      str.offsets[8] = 142
00000214  97 00 00 00                                      str.offsets[9] = 151
00000218  a4 00 00 00                                      str.offsets[10] = 164
0000021c  b5 00 00 00                                      str.offsets[11] = 181
00000220  b8 00 00 00                                      str.offsets[12] = 184
00000224  bd 00 00 00                                      str.offsets[13] = 189
00000228  c7 00 00 00                                      str.offsets[14] = 199
0000022c  ce 00 00 00                                      str.offsets[15] = 206
00000230  d4 00 00 00                                      str.offsets[16] = 212
00000234  e1 00 00 00                                      str.offsets[17] = 225
00000238  76 32 78 77 20 30 2e 34 2e 30 2b 39 66 30 36 34  str.blob = ['', 'v2xw 0.4.0+9f0649d', 'single-intersection', 'demo', 's_7f3a9c21', '/world/d172872bfdd10998babc1497334713546bbbb7dffa1b3b90fddb2d17d641e988.vwb', 'veh_0000', 'veh_0001', 'rsu_north', 'obu/cohda-mk5', 'rsu/cohda-mk5-rsu', 'car', 'truck', 'pedestrian', 'node.tx', 'phy.rx', 'gt.kinematics'] (padded to 228 B)
00000248  39 64 73 69 6e 67 6c 65 2d 69 6e 74 65 72 73 65  ...
00000258  63 74 69 6f 6e 64 65 6d 6f 73 5f 37 66 33 61 39  ...
00000268  63 32 31 2f 77 6f 72 6c 64 2f 64 31 37 32 38 37  ...
00000278  32 62 66 64 64 31 30 39 39 38 62 61 62 63 31 34  ...
00000288  39 37 33 33 34 37 31 33 35 34 36 62 62 62 62 37  ...
00000298  64 66 66 61 31 62 33 62 39 30 66 64 64 62 32 64  ...
000002a8  31 37 64 36 34 31 65 39 38 38 2e 76 77 62 76 65  ...
000002b8  68 5f 30 30 30 30 76 65 68 5f 30 30 30 31 72 73  ...
000002c8  75 5f 6e 6f 72 74 68 6f 62 75 2f 63 6f 68 64 61  ...
000002d8  2d 6d 6b 35 72 73 75 2f 63 6f 68 64 61 2d 6d 6b  ...
000002e8  35 2d 72 73 75 63 61 72 74 72 75 63 6b 70 65 64  ...
000002f8  65 73 74 72 69 61 6e 6e 6f 64 65 2e 74 78 70 68  ...
00000308  79 2e 72 78 67 74 2e 6b 69 6e 65 6d 61 74 69 63  ...
00000318  73 00 00 00                                      ...
--------
total: 772 bytes
```

Reading it back: `off_nodes = 256`, `off_classes = 352`, `off_channels = 424`, `off_world_ref = 448`,
`off_strings = 464`; the symbol table holds 17 strings (id 0 = `""`).

### 9.2 `Keyframe` — 180 bytes total (24 header + 156 body)

#### Keyframe — frame header (24 bytes)

```text
  offset  bytes                                            field
--------  -----------------------------------------------  ----------------------------------------
00000000  56 57 50 31                                      magic = 0x31505756 -> wire bytes "V" "W" "P" "1"
00000004  01 00                                            version = 1 (major)
00000006  02 00                                            msg_type = 0x0002 (Keyframe)
00000008  9c 00 00 00                                      body_len = 156 (uncompressed body bytes)
0000000c  00 00                                            flags = 0x0000
0000000e  00 00                                            reserved = 0
00000010  0a 00 00 00 00 00 00 00                          seq = 10
--------
total: 24 bytes
```


#### Keyframe — body (156 bytes, starts at frame offset 24)

```text
  offset  bytes                                            field
--------  -----------------------------------------------  ----------------------------------------
00000018  00 ca 9a 3b 00 00 00 00                          sim_time_ns = 1_000_000_000 (t = 1.0 s)
00000020  00 00 00 00 00 40 7f c0                          origin_x_m = -500.0
00000028  00 00 00 00 00 40 7f c0                          origin_y_m = -500.0
00000030  00 00 00 00 00 00 00 00                          origin_z_m = 0.0
00000038  03 00 00 00                                      actor_count = 3 (slot high-water + 1)
0000003c  01 00 00 00                                      signal_count = 1
00000040  40 00 00 00                                      off_actors = 64
00000044  94 00 00 00                                      off_signals = 148
00000048  01 00 00 00                                      gop_index = 1
0000004c  00 00                                            profile = 0 (full)
0000004e  00 00                                            reserved = 0
00000050  00 00 00 00 00 00 00 00                          reserved64 = 0
00000058  00 00 00 00                                      actors.actor_id[0] = 0
0000005c  01 00 00 00                                      actors.actor_id[1] = 1
00000060  02 00 00 00                                      actors.actor_id[2] = 2
00000064  59 d1 07 00                                      actors.x_mm[0] = 512345  (12.345 m)
00000068  00 53 07 00                                      actors.x_mm[1] = 480000  (-20.0 m)
0000006c  d8 ac 07 00                                      actors.x_mm[2] = 503000  (3.0 m)
00000070  96 94 07 00                                      actors.y_mm[0] = 496790  (-3.21 m)
00000074  14 a3 07 00                                      actors.y_mm[1] = 500500  (0.5 m)
00000078  42 42 08 00                                      actors.y_mm[2] = 541250  (41.25 m)
0000007c  2a 00 00 00                                      actors.lane_id[0] = 0x0000002a  [GT]
00000080  2b 00 00 00                                      actors.lane_id[1] = 0x0000002b  [GT]
00000084  ff ff ff ff                                      actors.lane_id[2] = 0xffffffff  [GT]
00000088  0f 00                                            actors.z_cm[0] = 15  (0.15 m)
0000008a  0f 00                                            actors.z_cm[1] = 15  (0.15 m)
0000008c  0a 00                                            actors.z_cm[2] = 10  (0.1 m)
0000008e  00 00                                            actors.heading_brad[0] = 0  (0.000000 rad)
00000090  00 80                                            actors.heading_brad[1] = 32768  (3.141593 rad)
00000092  00 40                                            actors.heading_brad[2] = 16384  (1.570796 rad)
00000094  f2 06                                            actors.speed_cq[0] = 1778  (13.89 m/s)
00000096  80 05                                            actors.speed_cq[1] = 1408  (11.0 m/s)
00000098  00 00                                            actors.speed_cq[2] = 0  (0.0 m/s)
0000009a  20 00                                            actors.accel_cq[0] = 32  (0.5 m/s2) [GT]
0000009c  b3 ff                                            actors.accel_cq[1] = -77  (-1.2 m/s2) [GT]
0000009e  00 00                                            actors.accel_cq[2] = 0  (0.0 m/s2) [GT]
000000a0  00                                               actors.class_idx[0] = 0
000000a1  01                                               actors.class_idx[1] = 1
000000a2  02                                               actors.class_idx[2] = 2
000000a3  08                                               actors.state[0] = 0x08
000000a4  09                                               actors.state[1] = 0x09
000000a5  00                                               actors.state[2] = 0x00
000000a6  07                                               actors.verified_neighbors[0] = 7
000000a7  05                                               actors.verified_neighbors[1] = 5
000000a8  00                                               actors.verified_neighbors[2] = 0
000000a9  00                                               actors.flags8[0] = 0
000000aa  00                                               actors.flags8[1] = 0
000000ab  00                                               actors.flags8[2] = 0
000000ac  07 00 00 00                                      signals.signal_id[0] = 7
000000b0  80 00                                            signals.time_to_change_ds[0] = 128 (12.8 s)
000000b2  03                                               signals.phase[0] = 3 (protected-green)
000000b3  00                                               signals.reserved[0] = 0
--------
total: 156 bytes
```

Body size check: `64 (prefix) + 28 × 3 (actors) + 8 × 1 (signals) = 64 + 84 + 8 = 156`. ✔
`off_actors = 64`, `off_signals = 148`.

### 9.3 `Delta` — 120 bytes total (24 header + 96 body)

One changed actor (slot 0, which advanced 1.389 m east and changed lane 42 → 44), no spawns, no despawns,
one signal countdown update.

#### Delta — frame header (24 bytes)

```text
  offset  bytes                                            field
--------  -----------------------------------------------  ----------------------------------------
00000000  56 57 50 31                                      magic = 0x31505756 -> wire bytes "V" "W" "P" "1"
00000004  01 00                                            version = 1 (major)
00000006  03 00                                            msg_type = 0x0003 (Delta)
00000008  60 00 00 00                                      body_len = 96 (uncompressed body bytes)
0000000c  00 00                                            flags = 0x0000
0000000e  00 00                                            reserved = 0
00000010  0b 00 00 00 00 00 00 00                          seq = 11
--------
total: 24 bytes
```


#### Delta — body (96 bytes, starts at frame offset 24)

```text
  offset  bytes                                            field
--------  -----------------------------------------------  ----------------------------------------
00000018  00 ab 90 41 00 00 00 00                          sim_time_ns = 1_100_000_000 (t = 1.1 s)
00000020  01 00 00 00                                      gop_index = 1
00000024  01 00 00 00                                      step_index = 1
00000028  01 00 00 00                                      moved_count = 1
0000002c  00 00 00 00                                      abs_count = 0
00000030  01 00 00 00                                      lane_count = 1
00000034  00 00 00 00                                      spawn_count = 0
00000038  00 00 00 00                                      despawn_count = 0
0000003c  01 00 00 00                                      signal_count = 1
00000040  40 00 00 00                                      off_moved = 64
00000044  00 00 00 00                                      off_abs = 0 (absent)
00000048  54 00 00 00                                      off_lanes = 84
0000004c  00 00 00 00                                      off_spawns = 0 (absent)
00000050  00 00 00 00                                      off_despawns = 0 (absent)
00000054  58 00 00 00                                      off_signals = 88
00000058  00 00 00 00                                      moved.slot[0] = 0
0000005c  6d 05                                            moved.dx_mm[0] = 1389 (+1.389 m since t=1.0 s)
0000005e  00 00                                            moved.dy_mm[0] = 0
00000060  00 00                                            moved.dz_mm[0] = 0
00000062  00 00                                            moved.heading_brad[0] = 0 (absolute)
00000064  f8 06                                            moved.speed_cq[0] = 1784 (13.9375 m/s, absolute)
00000066  20 00                                            moved.accel_cq[0] = 32 (0.5 m/s2, absolute) [GT]
00000068  08                                               moved.state[0] = 0x08 (EQUIPPED)
00000069  08                                               moved.verified_neighbors[0] = 8
0000006a  02                                               moved.mflags[0] = 0x02 (LANE_CHANGED)
0000006b  00                                               moved.reserved[0] = 0
0000006c  2c 00 00 00                                      lanes[0] = 44  [GT]  (for moved row 0, LANE_CHANGED set)
00000070  07 00 00 00                                      signals.signal_id[0] = 7
00000074  76 00                                            signals.time_to_change_ds[0] = 118 (11.8 s)
00000076  03                                               signals.phase[0] = 3 (protected-green)
00000077  00                                               signals.reserved[0] = 0
--------
total: 96 bytes
```

Body size check: `64 (prefix) + 20 × 1 (moved) + 0 (abs) + 4 × 1 (lanes) + 0 + 0 + 8 × 1 (signals)
= 64 + 20 + 4 + 8 = 96`. ✔ `off_moved = 64`, `off_abs = 0` (absent), `off_lanes = 84`,
`off_spawns = 0`, `off_despawns = 0`, `off_signals = 88`.

Applying the delta to the keyframe state:

```
slot 0:  x_mm = 512345 + 1389 = 513734  →  x = 513734/1000 + (−500) = 13.734 m
         y_mm = 496790 + 0    = 496790  →  y = −3.210 m
         z_cm = 15 + 0                  →  z = 0.15 m
         heading = 0 brad (absolute)    →  0.0 rad
         speed   = 1784 / 128           →  13.9375 m/s
         accel   = 32 / 64              →  0.5 m/s²
         state   = 0x08, verified nbrs  =  8
         lane    = 44  (MFLAG_LANE_CHANGED, first entry of the lane block)
slots 1, 2: unchanged
signal 7: phase 3, 11.8 s to change
```

### 9.4 `Delta` with a non-zero vertical delta — 128 bytes total (24 header + 104 body)

**This vector exists to pin one unit.** §3.2 defines the delta on all three axes as `i16`
millimetres against the previously transmitted quantised value, but the absolute vertical field is
centimetres everywhere it appears (§3.3.2, §3.4.3, §3.4.5), so "the same field" names a field in a
different unit from the delta. Every other worked example in §9 carries `dz_mm = 0`, so none of
them distinguishes the two readings, and a tenfold error survived in a real client because of it.
Prose cannot settle a unit; this vector can. A decoder that reads `dz_mm` as centimetres produces
`z = 1.150 m` for slot 0 below instead of `0.250 m` and fails on the first row.

It continues the worked example: it is the next canonical frame after §9.3, `seq = 12`, step 2 of
the same GOP, at `t = 1.2 s`. Slots 0 and 1 both entered the GOP at `z_cm = 15` (0.150 m) from the
§9.2 keyframe and were left there by §9.3, whose `dz_mm` is 0. Slot 0 rises by `dz_mm = +100` and
slot 1 falls by `dz_mm = −50`, so the vector pins the magnitude and the sign, and carries a
non-zero `dx_mm` alongside so the shared unit of the three axes is visible in one row.

#### Delta (vertical) — frame header (24 bytes)

```text
  offset  bytes                                            field
--------  -----------------------------------------------  ----------------------------------------
00000000  56 57 50 31                                      magic = 0x31505756 -> wire bytes "V" "W" "P" "1"
00000004  01 00                                            version = 1 (major)
00000006  03 00                                            msg_type = 0x0003 (Delta)
00000008  68 00 00 00                                      body_len = 104 (uncompressed body bytes)
0000000c  00 00                                            flags = 0x0000
0000000e  00 00                                            reserved = 0
00000010  0c 00 00 00 00 00 00 00                          seq = 12
--------
total: 24 bytes
```

#### Delta (vertical) — body (104 bytes, starts at frame offset 24)

```text
  offset  bytes                                            field
--------  -----------------------------------------------  ----------------------------------------
00000018  00 8c 86 47 00 00 00 00                          sim_time_ns = 1_200_000_000 (t = 1.2 s)
00000020  01 00 00 00                                      gop_index = 1
00000024  02 00 00 00                                      step_index = 2
00000028  02 00 00 00                                      moved_count = 2
0000002c  00 00 00 00                                      abs_count = 0
00000030  00 00 00 00                                      lane_count = 0
00000034  00 00 00 00                                      spawn_count = 0
00000038  00 00 00 00                                      despawn_count = 0
0000003c  00 00 00 00                                      signal_count = 0
00000040  40 00 00 00                                      off_moved = 64
00000044  00 00 00 00                                      off_abs = 0 (absent)
00000048  00 00 00 00                                      off_lanes = 0 (absent)
0000004c  00 00 00 00                                      off_spawns = 0 (absent)
00000050  00 00 00 00                                      off_despawns = 0 (absent)
00000054  00 00 00 00                                      off_signals = 0 (absent)
00000058  00 00 00 00                                      moved.slot[0] = 0
0000005c  01 00 00 00                                      moved.slot[1] = 1
00000060  6d 05                                            moved.dx_mm[0] = 1389 (+1.389 m)
00000062  b4 fb                                            moved.dx_mm[1] = -1100 (-1.100 m)
00000064  00 00                                            moved.dy_mm[0] = 0
00000066  00 00                                            moved.dy_mm[1] = 0
00000068  64 00                                            moved.dz_mm[0] = 100 = +0.100 m (MILLIMETRES, the same unit as dx_mm)
0000006a  ce ff                                            moved.dz_mm[1] = -50 = -0.050 m
0000006c  00 00                                            moved.heading_brad[0] = 0 (absolute)
0000006e  00 80                                            moved.heading_brad[1] = 32768 (pi rad, absolute)
00000070  f8 06                                            moved.speed_cq[0] = 1784 (13.9375 m/s, absolute)
00000072  80 05                                            moved.speed_cq[1] = 1408 (11.0 m/s, absolute)
00000074  20 00                                            moved.accel_cq[0] = 32 (0.5 m/s2, absolute) [GT]
00000076  b3 ff                                            moved.accel_cq[1] = -77 (-1.203125 m/s2, absolute) [GT]
00000078  08                                               moved.state[0] = 0x08 (EQUIPPED)
00000079  09                                               moved.state[1] = 0x09 (EQUIPPED|ATTACKER)
0000007a  08                                               moved.verified_neighbors[0] = 8
0000007b  05                                               moved.verified_neighbors[1] = 5
0000007c  00                                               moved.mflags[0] = 0x00 (no absolute entry, no lane change)
0000007d  00                                               moved.mflags[1] = 0x00
0000007e  00                                               moved.reserved[0] = 0
0000007f  00                                               moved.reserved[1] = 0
--------
total: 104 bytes
```

Body size check: `64 (prefix) + 20 × 2 (moved) + 0 + 0 + 0 + 0 + 0 = 64 + 40 = 104`. ✔
`off_moved = 64`; every other `off_*` is `0`, which per §2.2 means the section is absent — and
`moved_count` is the only non-zero count, so nothing is declared that is not placed.

**Applying it to the state §9.3 left behind.** The vertical column is the point of the vector, so
it is shown before and after in both the transmitted quantisation and in metres:

| Slot | z before | `dz_mm` | z after | as `z_cm` | x before | `dx_mm` | x after |
|---|---|---|---|---|---|---|---|
| 0 | 0.150 m (`z_cm = 15`) | `+100` | **0.250 m** | 25 | 13.734 m | `+1389` | 15.123 m |
| 1 | 0.150 m (`z_cm = 15`) | `−50` | **0.100 m** | 10 | −20.000 m | `−1100` | −21.100 m |

```
slot 0:  z = 15 cm × 10 = 150 mm;  150 + 100 = 250 mm  →  z = 0.250 m   (z_cm mirrors to 25)
         x_mm = 513734 + 1389 = 515123                 →  x = 515123/1000 + (−500) = 15.123 m
slot 1:  z = 15 cm × 10 = 150 mm;  150 − 50  = 100 mm  →  z = 0.100 m   (z_cm mirrors to 10)
         x_mm = 480000 − 1100 = 478900                 →  x = 478900/1000 + (−500) = −21.100 m
slot 2:  unchanged (not in the moved block)
```

The delta reference for the vertical axis is therefore the previously transmitted quantised value
**expressed in millimetres**, `z_cm × 10`, exactly as it is for `x` and `y`. A client that keeps its
vertical reference in centimetres and adds `dz_mm` into it is wrong by a factor of ten on every
vertical delta, and no other vector in this section would catch it.

### 9.5 Reference decoder skeletons

TypeScript (zero-copy view of an uncompressed keyframe):

```ts
const HDR = 24, MAGIC = 0x31505756;
function readKeyframe(frame: ArrayBuffer) {
  const dv = new DataView(frame);
  if (dv.getUint32(0, true) !== MAGIC) throw new Error("bad magic");
  if (dv.getUint16(4, true) !== 1) throw new Error("bad version");
  const body = HDR;                                  // uncompressed case
  const A   = dv.getUint32(body + 32, true);
  const off = body + dv.getUint32(body + 40, true);  // off_actors
  const origin = [dv.getFloat64(body + 8, true), dv.getFloat64(body + 16, true),
                  dv.getFloat64(body + 24, true)];
  let p = off;
  const actorId = new Uint32Array(frame, p, A); p += 4 * A;
  const xMm     = new Int32Array (frame, p, A); p += 4 * A;
  const yMm     = new Int32Array (frame, p, A); p += 4 * A;
  const laneId  = new Uint32Array(frame, p, A); p += 4 * A;
  const zCm     = new Int16Array (frame, p, A); p += 2 * A;
  const heading = new Uint16Array(frame, p, A); p += 2 * A;
  const speed   = new Int16Array (frame, p, A); p += 2 * A;
  const accel   = new Int16Array (frame, p, A); p += 2 * A;
  const cls     = new Uint8Array (frame, p, A); p += A;
  const state   = new Uint8Array (frame, p, A); p += A;
  const vnbrs   = new Uint8Array (frame, p, A); p += A;
  return { t: dv.getBigUint64(body, true), origin, A,
           actorId, xMm, yMm, laneId, zCm, heading, speed, accel, cls, state, vnbrs };
}
```

Rust (same frame, `bytemuck`):

```rust
const HDR: usize = 24;
const MAGIC: u32 = 0x3150_5756;

pub struct KeyframeView<'a> {
    pub t_ns: u64,
    pub origin: [f64; 3],
    pub actor_id: &'a [u32], pub x_mm: &'a [i32], pub y_mm: &'a [i32],
    pub lane_id: &'a [u32],  pub z_cm: &'a [i16], pub heading: &'a [u16],
    pub speed_cq: &'a [i16], pub accel_cq: &'a [i16],
    pub class_idx: &'a [u8], pub state: &'a [u8], pub verified_nbrs: &'a [u8],
}

pub fn read_keyframe(frame: &[u8]) -> Option<KeyframeView<'_>> {
    let g32 = |o: usize| u32::from_le_bytes(frame.get(o..o + 4)?.try_into().ok()?);
    let g64 = |o: usize| u64::from_le_bytes(frame.get(o..o + 8)?.try_into().ok()?);
    if g32(0) != MAGIC || u16::from_le_bytes(frame[4..6].try_into().ok()?) != 1 { return None; }
    let b = HDR;
    let a = g32(b + 32) as usize;
    let mut p = b + g32(b + 40) as usize;
    let mut take = |n: usize| { let s = &frame[p..p + n]; p += n; s };
    Some(KeyframeView {
        t_ns:   g64(b),
        origin: [f64::from_bits(g64(b + 8)), f64::from_bits(g64(b + 16)), f64::from_bits(g64(b + 24))],
        actor_id:      bytemuck::cast_slice(take(4 * a)),
        x_mm:          bytemuck::cast_slice(take(4 * a)),
        y_mm:          bytemuck::cast_slice(take(4 * a)),
        lane_id:       bytemuck::cast_slice(take(4 * a)),
        z_cm:          bytemuck::cast_slice(take(2 * a)),
        heading:       bytemuck::cast_slice(take(2 * a)),
        speed_cq:      bytemuck::cast_slice(take(2 * a)),
        accel_cq:      bytemuck::cast_slice(take(2 * a)),
        class_idx:     take(a), state: take(a), verified_nbrs: take(a),
    })
}
```

Both decoders produce, for the example keyframe:
`actor_id = [0,1,2]`, `x_mm = [512345, 480000, 503000]`, `y_mm = [496790, 500500, 541250]`,
`lane_id = [42, 43, 4294967295]`, `z_cm = [15, 15, 10]`, `heading = [0, 32768, 16384]`,
`speed_cq = [1778, 1408, 0]`, `accel_cq = [32, −77, 0]`, `class_idx = [0,1,2]`,
`state = [0x08, 0x09, 0x00]`, `verified_nbrs = [7, 5, 0]`.

---
## 10. Conformance checklist

An implementation claims "VWP v1 conformant" only if every box is ticked. `S` = server only, `C` = client
only, `B` = both. The test-kit ids match `tests/conformance/vwp/` (03-interfaces §17).

### 10.1 Framing

- [ ] **B · F1** Rejects a frame whose `magic ≠ 0x31505756` (close 1002).
- [ ] **C · F2** Ignores a frame with an unknown `msg_type` and keeps the connection.
- [ ] **B · F3** `body_len` always equals the uncompressed body length, compressed or not.
- [ ] **B · F4** All integers and floats are little-endian; a big-endian host produces identical bytes.
- [ ] **B · F5** Every array offset satisfies §2.2 alignment; a client can build typed-array views in place
      with no copy for an uncompressed frame.
- [ ] **B · F6** Reserved header and prefix bytes are written as zero and ignored on read.
- [ ] **S · F7** `Hello` is never compressed, even when longer than 4 KiB.
- [ ] **B · F8** zstd round-trips every frame type; `compress=none` is honoured.
- [ ] **C · F9** Unknown flag bits are ignored, not treated as errors.

### 10.2 Handshake, resume, backpressure

- [ ] **S · H1** `Hello` is the first frame and arrives within 1000 ms of the upgrade.
- [ ] **S · H2** The first canonical frame after a non-resumed `Hello` is a `Keyframe` with `FLAG_RESYNC`.
- [ ] **C · H3** Sends nothing before receiving `Hello`.
- [ ] **B · H4** `seq` is dense and monotonic across canonical frames; `Hello`/`Error`/`Bye` do not consume
      a `seq` and carry the next one.
- [ ] **S · H5** `?resume=<seq>` inside the ring resumes with `HELLO_RESUMED` and no gap.
- [ ] **S · H6** `?resume=<seq>` outside the ring falls back to `Hello` + `FLAG_RESYNC` keyframe, never an
      error.
- [ ] **S · H7** Under a client that reads at 1/10 the production rate, memory is bounded: queued bytes
      never exceed `max_queued_bytes`, and the sim loop never blocks. Test `bp_slow_client`.
- [ ] **S · H8** After a drop, a `Keyframe` with `FLAG_RESYNC` arrives within `resync_deadline_ms` of wall
      time, and a `stream.drop` notification names the gap.
- [ ] **S · H9** Deltas are dropped all-or-nothing; a client never receives a delta whose predecessor in
      the same GOP was dropped.
- [ ] **C · H10** Detects a `seq` gap without the notification and refuses to apply deltas until the next
      keyframe.
- [ ] **S · H11** Ping every 15 s; closes with 1001 after 30 s without a Pong.

### 10.3 Poses and state

- [ ] **B · Q1** Quantisation matches §3.2 exactly, including half-away-from-zero rounding and the brad
      wrap formula, for the vectors in §9.
- [ ] **S · Q2** Delta references are computed against the previously *transmitted quantised* value, so
      keyframe + all deltas reproduces the server's state bit for bit at every step of a GOP.
      Test `no_quantisation_drift`: 10,000 steps, assert max deviation ≤ 1 mm.
- [ ] **S · Q3** A displacement over 32,000 mm in one step sets `MFLAG_ABSOLUTE` and appears in the
      absolute block. Test `teleport_escape`.
- [ ] **B · Q4** Keyframe actor rows are indexed by slot, with `actor_id = 0xFFFFFFFF` for empty slots.
- [ ] **S · Q5** A slot is not reused until one full keyframe period after its despawn.
- [ ] **B · Q6** `state` bit semantics match §3.3.4; "benign" is the absence of bits 0–2.
- [ ] **C · Q7** Renders correctly when `lane_id` is `0xFFFFFFFF` and when `accel_cq` is 0 (node profile).

### 10.4 Content

- [ ] **B · C1** Every field of §3.5.2 is populated or explicitly set to its unknown sentinel; no field is
      silently zero.
- [ ] **B · C2** `Telemetry.record_size` is honoured for striding, not assumed to be 208.
- [ ] **B · C3** Unknown event `channel_id`s are skipped by `payload_len` without desynchronising.
- [ ] **B · C4** Event index arrays are sorted by `(sim_time_ns, channel_id)` and payloads are 8-aligned.
- [ ] **S · C5** Every `prov_id` referenced by a `MetricSample` or an event payload has been delivered in
      a `Provenance` frame before it is first referenced.
- [ ] **C · C6** `explain` on any displayed value resolves without an "unknown provenance" fallback.
- [ ] **B · C7** The symbol table is append-only within a connection and resets on a non-resumed `Hello`.

### 10.5 World

- [ ] **S · W1** `GET /world/{hash}.vwb` returns a body whose SHA-256 equals `{hash}` and
      `Hello.world_hash`.
- [ ] **S · W2** COOP/COEP/CORP headers are present on every HTTP response and on the upgrade.
- [ ] **C · W3** Verifies the world hash and refuses a mismatch.
- [ ] **B · W4** The JSON and binary forms of a world carry the same lanes, buildings, junctions, signals,
      sites and bbox. Test `world_json_binary_parity`.
- [ ] **S · W5** `world.generate` with the same params and seed produces the same `world_hash` on every
      platform.
- [ ] **C · W6** Handles `world_ref.mode = 1` (WorldChunk) and `mode = 2` (already have it).

### 10.6 Visibility

- [ ] **S · V1** In `profile=node`, none of the fields or channels of §5.2 appear, checked field by field
      by the `node_profile_leakage` test, which decodes the whole stream and asserts every GT field is at
      its sentinel and every GT channel is absent.
- [ ] **S · V2** In `profile=node`, actors without `ST_EQUIPPED` occupy empty slots.
- [ ] **S · V3** `events.set` on a GT channel, `metrics.query` on a GT metric, and `overlay.set` on a
      `*_gt` overlay all return `-32040`.
- [ ] **S · V4** The profile cannot be changed by any control method.
- [ ] **S · V5** A `full` recording replayed with `profile=node` yields byte-identical frames to a live
      `node` run of the same scenario. Test `node_profile_replay_parity`.
- [ ] **S · V6** Every exporter's output passes the leakage linter, and GT and NODE never share a file
      unless the exporter is declared `mixed` and the file is tagged.

### 10.7 Control surface

- [ ] **S · R1** All 32 methods of §6.15 are implemented and appear in `rpc.discover`.
- [ ] **S · R2** `rpc.discover` is a valid OpenRPC 1.3.2 document, and every params/result schema in it
      validates the corresponding example.
- [ ] **S · R3** Invalid params return `-32602` with a `data` array of `{path, message, hint}`.
- [ ] **S · R4** `run.seek` sends the keyframe and all deltas *before* the reply.
- [ ] **S · R5** `run.pause` has sent every frame up to and including the reply's `t_ns`.
- [ ] **S · R6** `run.step {unit:"event"}` advances exactly one DES event in `(time, priority, seq)` order.
- [ ] **S · R7** `POST /rpc` accepts the same requests; connection-scoped methods return `-32009`.
- [ ] **S · R8** Operations that exceed 2 s return a `Job` and emit `job.progress` / `job.done`.
- [ ] **C · R9** Tolerates unknown result properties and unknown notification methods.
- [ ] **S · R10** JSON-RPC batch arrays are rejected with `-32600`.

### 10.8 Replay

- [ ] **B · P1** `golden_live_vs_replay`: every canonical frame is byte-identical after masking
      `TRANSPORT_FLAG_MASK`.
- [ ] **S · P2** `bench_seek`: p95 ≤ 100 ms, max ≤ 200 ms over 100 random seeks on `downtown-1km2`;
      cold first seek ≤ 150 ms.
- [ ] **S · P3** The recorder stores whole frames with transport flag bits cleared.
- [ ] **S · P4** Seeking to `t` produces a keyframe with `sim_time_ns ≤ t` and at most
      `keyframe_period_ns / mobility_step_ns` deltas.
- [ ] **S · P5** The WASM reader emits the same stream as the native reader for the same file.
- [ ] **S · P6** A recording with a higher major is refused (`-32050`); a higher minor is accepted.

### 10.9 Versioning

- [ ] **B · N1** A v1 reader parses a synthetic v1.1 stream that adds a message type, a channel id, a
      telemetry field (larger `record_size`) and a new JSON-RPC method, losing only the new information.
- [ ] **S · N2** The subprotocol token is `vwp.v1` and an unknown one fails the upgrade with 426.
- [ ] **C · N3** Accepts `version_major ∈ {N, N−1}` and refuses anything else with close 4406.

---

## Appendix A — Enum reference

Collected for implementers; each is also defined at its point of use.

| Enum | Values |
|---|---|
| `NodeKind` | 0 obu, 1 vru-device, 2 rsu, 3 base-station, 4 router, 5 backend-entity, 6 other |
| `ActorCategory` | 0 vehicle, 1 vru, 2 infrastructure, 3 other |
| `Visibility` | 0 GT, 1 NODE, 2 PUBLIC, 3 MIXED, 4 DERIVED, 5 META |
| `WorldRefMode` | 0 http-by-hash, 1 inline chunks, 2 client-already-has |
| `SignalPhase` | SAE J2735 `MovementPhaseState` 0–9 (§3.3.3) |
| `SpawnCause` | 0 demand, 1 scenario-event, 2 respawn, 3 handover-in, 0xFFFF unknown |
| `DespawnCause` | 0 trip-end, 1 left-map, 2 parked, 3 scenario-event, 4 error, 0xFFFF unknown |
| `MsgType` | §3.6.3, 0–18 |
| `RxOutcome` | 0 ok, 1 below-sensitivity, 2 collision, 3 capture-loss, 4 half-duplex, 5 sinr-fail, 6 crc-fail, 7 not-a-candidate |
| `LossCause` | 0 none, 1 path-loss, 2 shadowing, 3 fading, 4 interference, 5 hidden-terminal, 6 half-duplex, 7 dcc-gate, 8 queue-drop, 9 out-of-range |
| `LosClass` | 0 LOS, 1 NLOSb, 2 NLOSv, 3 NLOSt, 4 NLOSbv, 0xFF withheld |
| `VerifyOutcome` | 0 valid, 1 invalid-signature, 2 revoked, 3 expired, 4 unknown-signer, 5 skipped, 6 dropped, 7 permission-denied |
| `AdmitDecision` | 0 now, 1 deferred, 2 skipped, 3 evicted |
| `Primitive` | §3.6.6, 0–10 |
| `CertEvent` | 0 change, 1 expire, 2 topup-request, 3 topup-complete, 4 learn-p2pcd, 5 learn-full-cert, 6 install, 7 evict, 8 revoked-self-detected |
| `RevocationStage` | 05-protocols §8: 0 detect … 10 residual_harm (§3.6.10) |
| `DccState` | 0 RELAXED, 1 ACTIVE_1, 2 ACTIVE_2, 3 ACTIVE_3, 4 RESTRICTIVE, 5 cv2x-cc, 0xFFFF n/a |
| `GnssFix` | 0 none, 1 2D, 2 3D, 3 DGNSS, 4 RTK-float, 5 RTK-fix, 6 dead-reckoning |
| `NodeRunState` | 0 off, 1 booting, 2 active, 3 parked, 4 degraded, 5 down, 6 compromised (GT) |
| `VerifyPolicy` | 0 verify-all, 1 on-demand, 2 prioritized |
| `MetricAgg` | 0 sum, 1 mean, 2 p50, 3 p95, 4 p99, 5 ratio, 6 rate, 7 max, 8 min |
| `LaneType` | 0 drive, 1 bike, 2 sidewalk, 3 bus, 4 parking, 5 junction-internal, 6 crossing |
| `JunctionControl` | 0 none, 1 priority, 2 signal, 3 stop, 4 yield, 5 roundabout |
| `ByeReason` | 0 run-complete, 1 client-requested, 2 server-shutdown, 3 error, 4 superseded |

### WebSocket close codes

| Code | Meaning |
|---|---|
| 1000 | normal, after `Bye` |
| 1001 | liveness failure (no Pong) |
| 1002 | protocol error (bad magic, malformed frame) |
| 1003 | unacceptable data (client sent a binary frame) |
| 1011 | server internal error |
| 1012 | superseded by a newer connection |
| 1013 | client too slow for `stall_timeout_s` |
| 4404 | unknown run |
| 4406 | unsupported protocol version |
| 4401 | unauthorized |

## Appendix B — Message-type summary

| Id | Name | Dir | Cadence | Body size |
|---|---|---|---|---|
| `0x0001` | `Hello` | S→C | once per connection | 256 + 32N + 24C + 8K + 16 + strings |
| `0x0002` | `Keyframe` | S→C | `keyframe_period_ns` (1 s), + resync/seek | 64 + 28A + 8S |
| `0x0003` | `Delta` | S→C | `mobility_step_ns` (100 ms) | 64 + 20M + 12·abs + 4·lanes + 36P + 8D + 8S |
| `0x0004` | `Telemetry` | S→C | `telemetry_period_ns` (1 s), subscribed nodes | 32 + 208·N |
| `0x0005` | `Event` | S→C | per mobility step, subscribed channels | 32 + 16E + payloads |
| `0x0006` | `MetricSample` | S→C | `metric_period_ns` (1 s) | 32 + 32M |
| `0x0007` | `Provenance` | S→C | after the first keyframe, then on demand | 32 + 24P + 8Dk + strings |
| `0x0008` | `WorldChunk` | S→C | only when `world_ref.mode = 1` | 64 + ≤ 1 MiB |
| `0x00FE` | `Error` | S→C | on stream error | 32 + strings |
| `0x00FF` | `Bye` | S→C | once, last | 32 + strings |

Bandwidth at the design point (10,000 actors, 1 s keyframes, 100 ms deltas, no event subscriptions):
280 KB/s of keyframes + 9 × ~150 KB/s of deltas ≈ 1.6 MB/s raw, ≈ 400–550 KB/s after zstd.

## Appendix C — Decision log

Every place this document made a choice the design documents left open, or corrected.

| # | Decision | Reason |
|---|---|---|
| 1 | Subprotocol token `vwp.v1` carries the major version | a mismatched client fails at the handshake, not at the first frame |
| 2 | Binary = telemetry, text = JSON-RPC, no in-band discriminator | the WebSocket framing layer already separates them, for free |
| 3 | Client sends nothing first; server sends `Hello` within 1 s | one less round trip before pixels |
| 4 | 24-byte header with `seq` in it | resume needs a sequence number on every frame, and 24 is 8-aligned |
| 5 | Magic `0x31505756` = wire bytes `VWP1` | greppable in a hex dump |
| 6 | **Flat fixed-layout struct-of-arrays, not FlatBuffers** (amends ADR 0008) | zero *parsing*, not just zero copy; one table beats three code generators; the recorder stores the same bytes |
| 7 | Resume ring = `max(2 GOPs, 8 MiB)`, ≤ 4096 frames | two GOPs guarantee a retained keyframe before any resume point |
| 8 | Backpressure: drop all deltas → drop P3 oldest-first → coalesce keyframes → `FLAG_RESYNC` keyframe within 250 ms | bounded memory, and a keyframe is idempotent so synthesising one is always legal |
| 9 | Drops reported by JSON-RPC notification, not in a binary field | keeps binary frames byte-identical live vs replay |
| 10 | zstd only, level 3, only for bodies ≥ 4 KiB | same codec as the MCAP chunks; level 3 encodes a 300 KB keyframe in under 1 ms |
| 11 | **Keyframe positions are `i32` mm, not `i16` mm** (corrects ADR 0008) | `i16` mm spans ±32.767 m and cannot address a 1 km² world from one origin; `i16` mm is right for *deltas*, and is kept there |
| 12 | Keyframe origin = `floor(bbox_min)`, constant for the run | `i32` mm reaches ±2,147 km, so re-centring is never needed; the field stays per-keyframe for future minor versions |
| 13 | Delta positions are relative to the previous frame of the GOP, computed against the **quantised** reference | no accumulated drift; GOP ordering is already guaranteed |
| 14 | `MFLAG_ABSOLUTE` escape for displacements > 32 m | removes the only failure mode of `i16` deltas |
| 15 | Heading = `u16` binary radians (65536/turn) | wraps by construction, 2 bytes, 0.0055° |
| 16 | Speed `i16` at 1/128 m/s, acceleration `i16` at 1/64 m/s² | power-of-two scales are exact; ±256 m/s and ±512 m/s² cover everything |
| 17 | World geometry in `f32` ENU metres | ulp is 0.12 mm at 1 km and 1.2 mm at 10 km *because* coordinates are world-local; the "f32 gives 0.1 m" worry applies to projected absolutes, which never appear on the wire |
| 18 | Keyframe actor rows are a dense array indexed by slot | matches the UI's `SharedArrayBuffer` pose rings keyed by slot (09-ui §2) |
| 19 | World fetched by HTTP GET on a content hash; `WorldChunk` only for static/WASM hosting | immutable, cacheable, shared between runs, and it does not sit behind the telemetry stream |
| 20 | World v1 keeps outer building rings only; holes dropped at import and recorded in provenance | neither the obstacle model nor the extruded-footprint renderer uses them |
| 21 | Lane connectivity and conflict matrices are not in the world payload | the UI never needs them; they would roughly double the payload |
| 22 | `Telemetry` and `MetricSample` are arrays of structs with a wire `record_size`; poses are struct-of-arrays | consumers read telemetry one node at a time; `record_size` is the cheapest additive-evolution path |
| 23 | `NODE-only` keeps the pose of *equipped* actors and drops unequipped ones entirely | an unequipped actor transmits nothing, so nothing node-visible can know it; equipped actors' positions are what their own broadcasts assert |
| 24 | `NODE-only` withholds `proto.revocation` stages 0–4 | stages before `issued` are internal to the MA and would leak the answer |
| 25 | Profile is fixed at connect and immutable | a profile that can be toggled is not a blind evaluation |
| 26 | `rpc.discover` returns OpenRPC 1.3.2 | the standard self-description for JSON-RPC; the copilot registry, the CLI and the Python stubs generate from it |
| 27 | No JSON-RPC batching | ordering against the binary stream gets murky and no caller needs it |
| 28 | No event channel is subscribed by default | `phy.rx` alone is millions of records per simulated second |
| 29 | Any operation over 2 s returns a `Job` + notifications | the socket is for the stream, not for waiting |
| 30 | MCAP message payload = the whole frame, header included | makes byte-identity a memcpy rather than an argument |
| 31 | Seek emits synthetic `Telemetry` + `Provenance` with `FLAG_SEEK_RESULT`, outside the byte-identity guarantee | these channels are latest-at, so a seek must reconstruct them |
| 32 | Client supports majors N and N−1; unknown minors are parsed and the extra ignored | matches 02-architecture §9 and makes minor bumps free |
| 33 | Fields may only be added into reserved bytes, a `record_size` tail, or a new `off_*` section | one rule, mechanically checkable, no "optional" wire fields |
| 34 | **`content_hash` is the SHA-256 of the body with its own 32 bytes zeroed** (§4.2 amendment, 2026-09-18) | a digest stored inside the data it covers has to exclude itself somehow; zeroing the field is what both independent implementations already did, and it needs no second pass over the body |

## Appendix D — Things this document invented

The design documents did not specify these; they are new here and should be reflected back into the ADRs
if accepted.

1. **The whole binary layout.** ADR 0008 named the message types; every byte offset, type, unit, enum and
   sentinel in §2–§4 is new, as is the decision (#6) to drop FlatBuffers for v1.
2. **The slot model.** 09-ui §2 mentions "actor slot" in passing; slot assignment (lowest free),
   release-after-one-keyframe, dense-array indexing and the delta reference semantics are new.
3. **`seq`, the resume ring and the reconnect state machine.** Nothing in the design covers reconnect.
4. **The backpressure policy** (priority classes, drop order, coalescing, synthesised keyframes,
   `stream.drop`, `stall_timeout_s`). The design only says the seek must be fast.
5. **The symbol table.** An interning scheme for detector ids, metric names, app ids and flow ids, with
   append-only `Provenance` extensions.
6. **The `vwp-world/1` format** (§4), including the JSON mirror and the hole-dropping rule.
7. **Event payload layouts** (§3.6.4–§3.6.17). 03-interfaces §14 lists key *field names* per channel; the
   types, units, enums, sizes and `msg_id` join key are new.
8. **The `NodeTelemetry` binary record** (§3.5.2). 06-node-models §2.4 lists the quantities in prose; every
   type, unit, scale (per-mille, centi-dBm, KiB) and sentinel is new.
9. **The exact GT field list and the `node`-profile blanking rules** (§5.2). The design says GT channels
   are tagged and stripped; which *columns* of the mixed snapshot channel are GT was open.
10. **The JSON-RPC schemas** (§6). 09-ui §8 and 02-architecture §9 list method *names*; every params
    schema, result schema and error code is new, as are `run.resume`, `run.status`, `events.set` filters,
    `Job` semantics and the eight server→client notifications.
11. **Error-code numbering** in the −32000…−32099 application range.
12. **The seek budget breakdown** (§7.4) and the `bench_seek` pass criteria. The design states the 100 ms
    target and says CI must guard it; the per-step budget is new.
13. **The versioning rules** (§8), in particular the "reserved bytes, `record_size` tail, or new `off_*`"
    rule and the breaking/additive classification.
14. **The conformance checklist** (§10) and the test ids.
15. **Two corrections to the design**: the `i16`-mm keyframe quantisation (decision #11) and the FlatBuffers
    choice (decision #6). Both should be recorded as amendments to ADR 0008.
