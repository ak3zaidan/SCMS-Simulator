# ADR 0009 — UI stack

- **Status:** Proposed (2026-09-18)
- **Related:** `09-ui.md`, ADR 0008. Evidence: UI research sheet (R9); all library facts cite npm registry metadata and project docs accessed 2026-09-17.

## Context

We need a bird's-eye 2D map that becomes a 3D follow view of one vehicle by a camera move, instanced rendering of thousands of actors at 60 fps on integrated GPUs, live sparklines, publication-quality post-run figures, a schema-driven configuration editor, and packaging that works served from the engine, statically (WASM), and later as a desktop app. Everything must be MIT/Apache/BSD-licensed.

## Decision

| Concern | Choice | Alternatives considered | Reason |
|---|---|---|---|
| 3D engine | **Three.js** (MIT, r186) with `InstancedMesh` per class × LOD and `BatchedMesh` for buildings; `WebGLRenderer` now, `WebGPURenderer` behind a flag | Babylon.js (Apache-2.0; thin instances fast but all-or-nothing culling [R9]); PlayCanvas (MIT; no frustum culling of instances [R9]) | largest ecosystem, the brief's default, jevpilot reference behaviours, `three-mesh-bvh` picking |
| 2D layer | **the same Three.js scene** viewed top-down by a perspective camera (orthographic toggle for measurement) | deck.gl (MIT; 1 M points at 60 fps [R9], but no documented context sharing with Three.js, so two renderers and a scene switch); PixiJS (MIT; 1 M particles at 60 fps on an M3 [R9], again a second renderer); MapLibre GL (BSD-3; documented Three.js custom layer, useful later for basemap tiles) | one world model, one scene, and the 2D→3D transition becomes a camera animation; deck.gl's GPU aggregation ideas are re-implemented as data textures |
| Live plots | **uPlot** (MIT) | Chart.js, ECharts, Plotly | the only candidate with published 60 fps streaming figures (3,600 points at 10 % CPU / 12 MB) [R9] |
| Post-run figures | **Plotly.js** (MIT) with SVG/PNG export | ECharts (Apache-2.0; SVG via `renderToSVGString`), Vega-Lite (BSD-3; `toSVG`), Observable Plot (ISC) | one library shared with the Python side (`plotly` in notebooks) and `toImage` SVG export [R9]; ECharts remains an acceptable substitute if bundle size matters |
| App framework | **React 19 + TypeScript + Vite 8**, Zustand state; render loop and plots outside React | Svelte 5, SolidJS (both faster in js-framework-benchmark medians: create-10k 228–231 ms vs 389 ms for React hooks [R9]) | the dashboard's hot paths are imperative (Three.js, uPlot, workers) so framework overhead is not on the critical path; React's ecosystem for schema-driven forms and the copilot panel weighs more; the benchmark gap is real and recorded |
| Workers/transport | decoder Web Worker owning the WebSocket, `SharedArrayBuffer` pose rings with `Atomics`, `ArrayBuffer` transfer fallback, optional `OffscreenCanvas` render worker | main-thread decoding | keeps 60 fps independent of stream bursts; requires COOP/COEP from the server [R9: MDN] |
| Packaging | served by the engine (`v2xw serve`), static WASM mode, **Tauri 2** shell later (Apache-2.0/MIT) | Electron (MIT; 8-week major cadence, Chromium bundled [R9]) | Tauri's small footprint; webview parity risk noted, Electron as fallback |

## Consequences

- No second rendering engine; overlays such as CBR heatmaps are Three.js shader planes fed by engine data textures.
- The exact-orthographic mode cannot animate into 3D (Three.js does not interpolate projections); the default map mode uses a top-down perspective camera so the fly-down is continuous.
- WebGPU is opt-in until vendors stop labelling their WebGPU paths experimental [R9].
- jevpilot has no licence file; only its ideas (kinematic bicycle model, background worker planner, camera smoothing, render profile) are reused, never its code [R9: GitHub API `license: null`].
