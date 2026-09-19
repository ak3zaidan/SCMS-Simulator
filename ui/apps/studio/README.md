# `@vwp/studio`

The Studio: the React shell of the V2X World Simulator UI (design/09-ui §6). It consumes
`@vwp/protocol` (VWP v1 client) and `@vwp/viewer` (Three.js scene) and adds nothing to the wire —
every engine interaction is a method from `docs/protocol/vwp-v1.md` §6.

```sh
# from ui/
node packages/mock-server/dist/index.js --actors 200 --port 8787
pnpm --filter @vwp/studio dev          # http://127.0.0.1:5173
```

`VWP_ENGINE` (default `http://127.0.0.1:8787`) picks the engine the dev server proxies to;
`VWP_STUDIO_PORT` the port it listens on.

## Layout

The shell is the wireframe of 09-ui §6, as a CSS grid:

| Region | Component | What it is |
|---|---|---|
| left | `ScenarioPanel`, `RunBrowser`, `ComparisonView`, `CopilotPanel` | scenario form, run manifests, comparison stub, tool surface |
| centre | `Viewport` + `TimeControls` | one canvas, the overlay/camera toolbar, transport and scrub bar |
| right | `Inspector` (`state · why · log`) | the §3.5.2 record in full, provenance, the connection log |
| bottom | `PlotsStrip` | live `MetricSample` series in uPlot |
| floating | `ObuHud` | the HUD of 09-ui §5, dockable into the inspector |

## How it is wired

`src/state/engine.ts` owns everything hot: the `VwpClient`, the `Viewer`, the decoded world, the
`prov_id` dictionary, the telemetry rings and the metric history. React sees a **projection** of that
state, refreshed at 5 Hz (`src/state/store.ts`, Zustand) — 09-ui §4's "HUD updates 5 Hz for DOM".
Poses, interpolation and instance writes never touch React.

The endpoints are relative, because the engine serves the built Studio in production (09-ui §9):
`/vwp/v1` for the stream, `/rpc` for JSON-RPC over HTTP, `/world/{hash}.vwb` for the world.
`vite.config.ts` proxies those onto the engine in development, which also keeps the app same-origin —
the engine sets `cross-origin-resource-policy: same-origin` on every response (§1.1), so a
cross-origin world fetch would be blocked.

Four protocol details drive most of the behaviour:

- **`view.follow` is the telemetry subscription** (§6.7). Clicking an actor picks it, flies the camera
  down (`Viewer.flyTo`), and calls `view.follow`, which is what makes `Telemetry` frames start
  arriving for that node — and therefore what fills the HUD.
- **Channels start unsubscribed** (§6.12). The app subscribes `node.tx`, `phy.rx`, `mac.cbr`,
  `sec.cert`, `det.observation`, `app.warning` and `proto.revocation` at connect; the tx-pulse and
  link overlays and the scrub bar's event markers are fed from those frames.
- **`Provenance` (§3.8) is the "why" tab's fast path.** Values that carry a `prov_id` resolve with no
  round trip; values that do not — every `NodeTelemetry` field, since §3.5.2 has no `prov_id`
  column — say so explicitly and offer `explain` (§6.9).
- **Sentinels are not numbers.** `0xFFFF`, `0xFFFF_FFFF`, `u64::MAX` and `NaN` mean "not modelled at
  this tier" (§3.5.2) and render as `n/a`, never as 65,535.

## Known gaps, honestly

- The HUD sketch in 09-ui §5 shows an **evidence-buffer count** and a **last CRL fetch** time that the
  §3.5.2 record does not carry. They are rendered as explicit gaps with the reason, and read from
  `inspect.node`'s optional `stores`/`crl` sections when the engine provides them.
- **Comparison** is a stub: `metrics.query {runs:[…]}` needs several runs and `experiment.compare` is
  reserved for a later minor version (§6.15).
- **Copilot** is a tool surface, not a chat: the method list comes from `rpc.discover` so it cannot
  drift, but no model is connected.
- The **scenario form** falls back to a built-in Phase 1 field list when the engine does not publish
  `scenario-1.json` on `scenario.get {with_schema:true}`; the panel says which source it is using.

## Verifying

```sh
pnpm --filter @vwp/studio typecheck
pnpm --filter @vwp/studio lint
pnpm --filter @vwp/studio build
pnpm --filter @vwp/studio exec playwright install chromium
pnpm --filter @vwp/studio test:unit                  # test/ — Node, no browser, ~1 s
VWP_ACTORS=200 pnpm --filter @vwp/studio test        # unit + e2e + screenshots/
VWP_HEADED=1 VWP_ACTORS=5000 pnpm --filter @vwp/studio exec playwright test e2e/perf.spec.ts
```

`playwright.config.ts` starts both the mock engine and the dev server. Headless chromium has no GPU
here, so WebGL falls back to SwiftShader and the *presented* frame rate is a software-rasteriser
figure; `VWP_HEADED=1` opens a real window and measures on the machine's GPU. Results land in
`screenshots/fps.txt`.

Two suites, split by what they can actually prove:

- `test/` runs in Node and measures the store projection — how many Zustand notifications a stream
  of `MetricSample`/`Event` frames causes, how often each selector's value changes identity (which
  is what decides a `useStudio(selector)` re-render), how many node positions a disabled overlay
  reads, and whether `MetricHistory.push` is O(1) at capacity.
- `e2e/` drives the real browser for the things a simulation cannot reach: React's synthetic
  `onChange` on an `<input type="range">` (one `run.seek` per drag, not one per step), the tab order
  and keyboard activation of the HUD's telemetry buttons, `:focus-visible` on the reset-styled
  labels, and the overlay state the app opens in.
