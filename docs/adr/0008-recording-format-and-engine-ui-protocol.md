# ADR 0008 — Recording/replay format and engine-to-UI protocol

- **Status:** Proposed (2026-09-18)
- **Related:** `02-architecture.md` §9, `08-measurement-and-data.md` §5, `09-ui.md` §7. Evidence: UI research sheet (R9).

## Context

The UI must attach to a live run or replay a recording with sub-100 ms scrubbing; the same typed records must feed exporters, metrics, and the replay reader; datasets must be Parquet/JSONL; the protocol must be versioned and language-neutral (Rust engine, TypeScript UI, Python tools).

## Decision

1. **Recording container: MCAP** (MIT; libraries in Rust, Python, TypeScript, C++; chunked, indexed, append-only, self-describing schemas; default rosbag2 format since ROS 2 Iron) [R9: mcap.dev spec; foxglove/mcap]. Channels = the event families in 03-interfaces §14, each with a FlatBuffers schema record; chunk compression zstd; keyframes every 1 s of simulated time on `snapshot.keyframe`, deltas per mobility step on `snapshot.delta`.
2. **Event encoding: a flat fixed-layout struct-of-arrays** carried in MCAP message payloads (see the amendment below); **Arrow IPC** (Apache-2.0) for tabular metric batches and for Python plug-in exchange; **Parquet** for exported tables; JSONL retained for the legacy MA dataset profile.
3. **Engine-to-UI protocol "VWP v1":** WebSocket; binary frames are the same FlatBuffers tables as the recording (`Hello`, `Keyframe`, `Delta`, `Telemetry`, `Metric`, `Provenance`, `Event`); a JSON-RPC 2.0 control channel (text frames or HTTP) carries commands. The live engine and the replay reader emit identical streams; the UI supports versions N and N−1 via `Hello.version`.
4. **Seek contract:** keyframe + ≤ 1 s of deltas; poses quantised as in the amendment below; the replay reader (native or WASM) uses MCAP's chunk index and per-channel message index [R9: mcap.dev spec].

## Amendments (2026-09-18, from the VWP v1 wire specification)

Writing `docs/protocol/vwp-v1.md` against this ADR exposed two defects in it. Both are corrected there and the specification is authoritative where the two disagree.

- **Pose quantisation was impossible as written.** Int16 millimetres spans ±32.767 m, which cannot address a square-kilometre world from a single per-keyframe origin. Corrected: **keyframe** positions are `i32` millimetres relative to an origin that is constant for the run but is carried in **every** keyframe, and `i16` centimetres for z. Saying only "per-run" is the lossy shorthand: the wire specification §3.3.1 requires a client to read the origin from each keyframe rather than cache it from the handshake, so that a later minor version can re-centre without a format change. A reader who caches it violates that requirement; **delta** positions are `i16` millimetres relative to the previously transmitted quantised value, which is where the original int16 intent belongs (3.6 m of travel per 100 ms step at 130 km/h fits with room to spare), with an absolute-escape flag for teleports so there is no failure mode. Quantising against the previously *transmitted* value, not the true value, keeps the error from accumulating. Headings are `u16` binary radians, which wrap by construction.
- **FlatBuffers is dropped for v1.** A hand-specified flat layout gives zero *parsing* rather than merely zero copy, needs no schema compiler pinned across three build systems, and lets the recorder store exactly the bytes that went over the wire, which is what makes live and replay streams provably byte-identical. The cost is that field addition is manual and governed by explicit versioning rules. Revisit if the channel set grows faster than the layout table can be reviewed.

## Alternatives

| Container | Random access | Streaming/append | Libraries (Rust/TS/Py) | Licence | Verdict |
|---|---|---|---|---|---|
| MCAP (chosen) | chunk + message index | yes | yes/yes/yes | MIT | chosen |
| Parquet only | column ranges, not time-indexed events | poor for append | yes/yes/yes | Apache-2.0 | used for tables, not the event log |
| SQLite | good | ok | yes/yes/yes | public domain | rejected: the rosbag2 experience of write vs resilience trade-offs [R9: foxglove blog] |
| Custom binary + index | full control | yes | write ourselves | — | rejected: reinventing MCAP |
| Rerun `.rrd` | good via chunk store | yes | Rust/Py/C++ (no TS reader) | MIT/Apache-2.0 | rejected: no TypeScript reader, format has limited backward compatibility [R9: Rerun ARCHITECTURE.md]; its latest-at/range query-cache idea is adopted in the reader |

| Wire encoding | Decode cost in JS | Zero-copy | Schema evolution | Licence | Verdict |
|---|---|---|---|---|---|
| FlatBuffers (chosen) | none (in-place access) | yes | optional fields, deprecation | Apache-2.0 | chosen |
| Cap'n Proto | none | yes | good | MIT; TS binding `capnp-es` is pre-1.0 (0.0.x) [R9] | rejected on TS maturity |
| MessagePack | parse per message (≈ JSON.parse speed for `@msgpack/msgpack`, up to ~4.6× with `msgpackr` records) [R9] | no | free-form | ISC/MIT | rejected for the hot path; allowed for control messages |
| Protobuf | parse per message | no | good | BSD | used for gRPC plug-ins only |
| Arrow IPC | none for columnar batches | yes | schema in stream | Apache-2.0 | chosen for metrics/tabular |
| XVIZ (GLB envelope, AVS) | parse GLB JSON + BIN | partly | stream-typed | Apache-2.0 | rejected as a wire format (deck.gl-centric), adopted as a design reference for stream naming, `future_states`, and declarative UI metadata [R9: XVIZ docs] |

## Consequences

- One `.fbs` schema set generates Rust, TypeScript and Python types; the recording, the live stream and the exporters cannot drift apart.
- The engine's HTTP server must send COOP/COEP headers so the UI can use `SharedArrayBuffer` (09-ui §2).
- Keyframe cadence is a scenario field; the seek benchmark in CI guards the ≤ 100 ms target.
- Ground-truth channels are tagged; a `NODE-only` replay profile strips them for blind demonstrations.
