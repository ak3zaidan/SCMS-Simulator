# `v2xw-wasm` — the replay reader in the browser

The Studio scrubs a recording with no server at all: the recording is a static file, the
reader is this WebAssembly module, and a seek is an HTTP range request or two.

There is **no reader in this crate.** `v2xw-record` holds the container, the seek index and
the frame decoders, and 09-ui §7 requires exactly one of them:

> The replay reader is the same Rust code compiled natively (served by
> `v2xw serve --replay file.mcap`) or to WASM (static hosting), and emits the identical VWP
> stream, so the Studio has no replay-specific code path.

So this crate compiles that reader for `wasm32-unknown-unknown` and adds the two things a
browser needs and a file system does not.

| Piece | Module | Why it is here and not in `v2xw-record` |
|---|---|---|
| A byte source fetched a range at a time | `ranges` | A browser cannot read synchronously; the ask-and-retry loop is a transport concern |
| Resolved pose columns, laid out for a zero-copy handoff | `scene` | A renderer wants the state *at* `t`, which is the keyframe with its deltas applied |
| The seek loop | `session` | Ties the two together; this is what the JavaScript class calls |
| The `wasm-bindgen` surface | `js` (`wasm32` only) | `wasm-bindgen` has no meaning on the host |

## Using it

```js
import { views, open, seek, httpRangeFetcher, contentLength } from './replay.js';
import init, { ReplayReader, wasmMemory } from './v2xw_replay.js';

await init();

// Either: the whole file.
const reader = ReplayReader.fromBytes(new Uint8Array(await (await fetch(url)).arrayBuffer()));

// Or: fetched a range at a time, which is the point of the chunk index.
const ranged = ReplayReader.ranged(await contentLength(url));
const fetchRange = httpRangeFetcher(url);
await open(ranged, fetchRange);
await seek(ranged, 12.5, fetchRange);

// The columns are views over the WebAssembly linear memory, not copies.
const v = views(ranged, wasmMemory());
v.xMm;              // Int32Array, millimetres east of `ranged.origin[0]`, one per slot
v.actorId;          // Uint32Array, 0xFFFFFFFF marks an empty slot
v.state;            // Uint8Array, the state byte of vwp-v1 §3.3.4
```

Every call that may need bytes returns a flat `Float64Array` of `[offset, len, …]`: empty
means it finished, non-empty means fetch those ranges, `supply` them and call again. A
recording opened with `fromBytes` never returns a non-empty one, so one loop serves both.

Views go stale when `reader.generation` changes — a column grew, or linear memory did and
detached the `ArrayBuffer`. `viewsFor(reader, memory, previous)` rebuilds only when it has
to.

## Building

```sh
scripts/build-wasm.sh                      # Node/CommonJS into target/wasm-pkg
scripts/build-wasm.sh --web out/studio     # an ES module for the browser
```

It needs `wasm-bindgen-cli` at the version this crate pins (`0.2.128`) and, less
obviously, **a `clang` that can target WebAssembly**:

* The recording container's compression codecs are C — `zstd-sys` and `lz4-sys`, reached
  through `mcap`, whose `zstd` and `lz4` features are both on by default.
* **Apple's `clang` has no `wasm32` back end.** `clang -print-targets` does not list it and
  `cc` fails with `No available targets are compatible with triple
  wasm32-unknown-unknown`. Homebrew's does: `brew install llvm`.
* `wasm32-unknown-unknown` has no sysroot, so `#include <string.h>` has nowhere to come
  from. `zstd-sys` ships a `wasm-shim/` directory of exactly those freestanding stubs for
  this reason; `lz4-sys` does not and needs the same ones, so the script puts the shim on
  the include path for both builds. `limits.h`, `stddef.h` and `stdint.h` come from clang
  itself.

None of that is a choice this crate made; it is what building a C codec for a target with
no C library costs. `scripts/build-wasm.sh` finds the toolchain and the shim, and says
what is missing if it cannot. Override with `WASM_CC`, `WASM_AR`, `WASM_LIBC_SHIM` and
`WASM_BINDGEN`.

`rust-toolchain.toml` names `wasm32-unknown-unknown` in `targets`, so `rustup` installs
the cross std automatically. That file is shared by the whole workspace.

## Testing and measuring

```sh
cargo test -p v2xw-wasm                                        # the session, natively
cargo run -p v2xw-wasm --example mkfixture -- target/wasm-fixture
node tests/node/smoke.mjs                                      # wasm vs native, one file
cargo run -p v2xw-wasm --release --example mkbench -- target/wasm-bench
node bench/seek.mjs                                            # the §7.4 seek budget
```

`cargo test` exercises the seek, the delta application and the range loop on the host
against a recording `v2xw-record`'s fixture wrote, compared against an independently
written row-wise applier. `tests/node/smoke.mjs` then holds the WebAssembly build to the
**native** build's answers on the same file — every keyframe time, delta count and pose
column, as an equality and not a tolerance, because vwp-v1 §3.2's delta reference rule
makes keyframe-plus-deltas land exactly on the transmitted integers.

`bench/seek.mjs` measures the seek against §7.4's 100 ms p95 budget. `mkbench` writes the
recording §7.4's budget is written against — 400 actors, 600 s, 4 MiB chunks — measures
the native build over a fixed list of 200 targets, and records both, so the two figures
differ only by the compilation rather than by the machine or the sample.

## Not in scope yet

* **A worker harness.** The reader is single-threaded by construction (`Rc<RefCell<…>>`
  over the byte cache), which is correct for a dedicated worker and for the main thread,
  and it is not `Send`. Moving a *recording* between workers means opening it in each.
* **`SharedArrayBuffer` pose rings.** 09-ui §2 has the UI write poses into a shared ring
  keyed by actor slot. The columns here are already slot-indexed and dense, so that is a
  `TypedArray.set()` at the call site; nothing in this crate needs to know about it.
* **Telemetry, event and metric channels.** A seek resolves poses and signals. The other
  channels are the serde/Parquet path of build decision D11 item 5 and are read through
  `v2xw-record`'s exporters, not through a scrub bar.
