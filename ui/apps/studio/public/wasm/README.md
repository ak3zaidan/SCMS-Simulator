# The WebAssembly replay reader

This directory is where the Studio looks for the reader that opens a recording with no engine
running (09-ui §7). It is empty in the repository on purpose: the module is a build output, and
building it needs a toolchain that not every machine has.

## Building it

```sh
crates/v2xw-wasm/scripts/build-wasm.sh --web ui/apps/studio/public/wasm
```

That writes `v2xw_replay.js` and `v2xw_replay_bg.wasm` here, which is exactly what
`ui/apps/studio/src/lib/replay.ts` loads (`DEFAULT_REPLAY_LOCATION`). Vite serves everything under
`public/` verbatim, so nothing else has to change — and that is the whole deployment story for the
no-server path: a static directory.

## Why it is not built automatically

The recording container's compression codecs are C (`zstd-sys` and `lz4-sys`, reached through
`mcap`), so building the reader for `wasm32-unknown-unknown` needs a `clang` with the WebAssembly
back end and a set of freestanding libc headers. Apple's `clang` has no `wasm32` target at all.
`crates/v2xw-wasm/scripts/build-wasm.sh` finds a suitable compiler, puts `zstd-sys`'s `wasm-shim`
on the include path and runs `wasm-bindgen`; its header explains each step and why it is needed.

Neither the codec nor the missing sysroot is a choice this project made, so the build stays a
deliberate step rather than a hidden one. The Studio's own build never depends on it: the module is
imported at runtime by URL, and when it is absent the panel that would have used it says how to
build it instead of failing.

## What it is, and is not

It is the same Rust reader the native engine uses, compiled for a second target — 09-ui §7's "one
reader, two targets" rule, which is why the stream it resolves is byte-identical to the live one
(vwp-v1 §7.2) and why the Studio has no replay-specific decoder.

It reads poses and signals. A recording carries no world payload and no `Hello` (§7.1), so the
geometry has to come from elsewhere — a `.vwb` file beside the recording, or the run the recording
is being compared against — and the Studio says which, rather than drawing one run's actors over
another run's streets.
