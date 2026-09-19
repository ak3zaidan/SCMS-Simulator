# V2X World Simulator — UI workspace

TypeScript workspace for the Studio UI and everything that talks VWP v1
(`docs/protocol/vwp-v1.md`) to the Rust engine.

| Package | What it is |
|---|---|
| `packages/protocol` (`@vwp/protocol`) | VWP v1 client: framing, zero-parse binary decoders, pose dequantisation, slot model, `vwp-world/1` parser, typed JSON-RPC client, `VwpClient`, Web Worker transport |
| `packages/mock-server` (`@vwp/mock-server`) | Node WebSocket + HTTP server that speaks the server side of VWP v1 against a synthetic Manhattan world; a development and test fixture, not production code |
| `packages/viewer` (`@vwp/viewer`) | Three.js viewer: one scene for the 2D map and the 3D views, instanced actors, per-tile world geometry, overlays, cameras and picking (framework-free) |

## Getting started

```sh
corepack prepare pnpm@10.15.0 --activate   # if pnpm is missing
pnpm install
pnpm -r typecheck
pnpm -r test
pnpm -r build

# run the mock engine
node packages/mock-server/dist/index.js --actors 500 --port 8787
```

The protocol package is environment-neutral (browser, worker, Node 22+); it has no
runtime dependencies. zstd is not bundled: a decompressor is injected through
`DecodeOptions.decompress` when a connection negotiates `compress=zstd`.

## Known spec issues

**§3.2 / §3.4.2 — the unit of the z delta (open erratum, needs the Rust implementer's agreement).**
`docs/protocol/vwp-v1.md` §3.2 defines *delta x, y, z* as `i16` **millimetres** "relative to the same
field in the previously delivered frame of the GOP", and §3.4.2 names the field `dz_mm`, with the
32,000 mm escape threshold (§3.2 "Escape hatch") applying to `|dz|`. But the *absolute* z of a
keyframe (§3.3.2), of a delta absolute block (§3.4.3) and of a spawn row (§3.4.5) is `z_cm`,
**centimetres**. "The same field" therefore names a field in a different unit from the delta, and a
reader can take the reference to be either centimetres or millimetres — a 10x error on every vertical
delta, which no worked example in §9 catches (every `dz_mm` there is 0).

This client resolves it the only way the stated units allow: **`dz_mm` is millimetres and the z
reference is millimetres**, i.e. `z_cm · 10`. `PoseBuffer` keeps `zMm` as the authoritative z column,
seeds it from every absolute `z_cm` source as `z_cm · 10`, advances it by `dz_mm`, and exposes `zCm`
as a derived mirror. That keeps the §3.2 delta reference rule ("quantisation error does not
accumulate") true on all three axes and keeps the escape threshold meaningful.

The spec should say so explicitly; until it does, a producer that emits `z_cm`-unit deltas in the
`dz_mm` field will be interoperable with nothing. Regression tests:
`packages/protocol/test/quantisation.test.ts`, describe block
"§3.2/§3.4.2 — `dz_mm` is millimetres, and the z reference is millimetres too", plus the §10.3 Q2
`no_quantisation_drift` test, whose z trajectory now has a vertical amplitude large enough for
`dz_mm` to be non-zero on > 90 % of its 10,000 steps.
