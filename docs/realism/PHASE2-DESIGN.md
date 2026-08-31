# Phase 2 design note — V2X channel realism

Binding decisions for the Phase 2 implementation agents, taken after reading
[investigation/research-net-sota.json](investigation/research-net-sota.json) and probing the host.
These narrow the roadmap's §3.2 sketch into an implementable contract.

## Host-derived constraints (verified 2026-08-30)

| Fact | Consequence |
|---|---|
| Installed: numpy 2.5.2, pandas 3.0.5, pyarrow 25.0.1, libsumo/traci/sumolib **1.25.0** (exact match to the SUMO binary) | Phase 3 mobility has a working substrate; no DLL-mismatch fix needed |
| **No** shapely, **no** scipy | Geometry must be pure numpy: segment-vs-polygon tests over a uniform spatial grid, mirroring the existing spatial hash at `run.py:2702`. Do **not** add a geometry dependency |
| `requirements.txt` is only cryptography/pydantic/pytest | numpy is already a de-facto runtime dep of `datagen`; keep `mock_pipeline` importable without new deps — guard optional imports |
| WSL feature **Disabled**, no distros; enabling needs a reboot | **No in-loop or offline ns-3 / ms-van3t / VaN3Twin federation.** MOSAIC's native-Windows SNS is *less* realistic than what we already have, so it is not adopted either |

Because we cannot run a network simulator to generate golden curves, published measurement and
analytical-model curves are pinned as reference data instead. The literature supports this: the
closed-form models below were themselves validated against full network simulators over wide
parameter ranges.

## The model stack (opt-in `radio_model="geometric"`)

Applied at the single reception choke point (`run.py:2686-2765`). Every stage is opt-in and drawn
from dedicated string-keyed RNG streams so the default-config golden digest is untouched.

**1. Link classification** — per (tx, rx) pair per step: LOS / NLOSv (blocked by a vehicle) /
NLOSb (blocked by a building). Buildings come from OSM `building=*` fetched through the existing
cached Overpass path in `osm.py`; vehicles are rectangles on the same edge. Synthetic maps with no
building data fall back to a per-edge urban-canyon density parameter.

**2. Large-scale path loss** — exact 3GPP TR 37.885 constants (f in GHz, d in m):

| State | Formula | Shadowing σ | Decorrelation |
|---|---|---|---|
| Urban LOS | `38.77 + 16.7·log10(d) + 18.2·log10(f)` | 3.0 dB | 10 m |
| Urban NLOS | `36.85 + 30·log10(d) + 18.9·log10(f)` | 4.0 dB | 13 m |
| Highway LOS | `32.4 + 20·log10(d) + 20·log10(f)` | 3.0 dB | 10 m |

NLOSv adds `max{0, N(μ,σ)}` where μ = 9.0 + max(0, 15·log10(d) − 41), σ = 4.5 dB when both antennas
are below the blocker; μ = 5.0 + …, σ = 4.0 dB when one is. Blocker heights: car 1.6 m, truck 3.0 m.
Empirical basis: measured single-vehicle blockage 5.5–17 dB.

Optional refinement: Sommer/Veins building attenuation (≈9 dB per exterior wall + 0.4 dB/m interior)
where per-wall counts are cheaper than full polygon tests.

**3. Shadowing** — replaces the current i.i.d. per-step draw (which is additionally keyed on the
**cert digest**, so pseudonym rotation resamples the channel — a correctness bug, roadmap G5).
Use an AR(1)/Gudmundson process per link, carried across steps, keyed on the **true vehicle id**:
`f"{seed}:shadow2:{tx_vid}:{rx_vid}"`.

**4. Small-scale fading** — per-packet Nakagami-m, m ≈ 3 / 1.5 / 1.0 over 0–50 / 50–150 / >150 m,
feeding an SINR→PER mapping for the 6 Mb/s QPSK-1/2 10 MHz 802.11p profile.

**5. Loss composition** — independent-survival product `p_deliver = Π(1−p_i)`. The current code adds
probabilities and can exceed 1.0 (roadmap G5).

**6. MAC / congestion without event simulation** — port the closed-form Sepulcre et al. (IEEE TVT
2022) 802.11p model: PDR(d) decomposed into below-sensing / propagation / collision (incl. hidden
terminal) errors, plus the first analytical CBR estimator accurate at high load. Inputs are local
density, beacon rate, power, data rate, packet size. This turns the flat `packet_loss_base` and the
linear `congestion` ramp into physics that responds to fleet density.

**7. ETSI DCC** — reactive state table on modeled CBR: <0.30 → 10 Hz, 0.30–0.40 → 5 Hz,
0.40–0.50 → 2.5 Hz, 0.50–0.60 → 2 Hz, >0.60 → 1 Hz. CBR is the share of 100 ms probes above
−85 dBm. Optional adaptive LIMERIC variant: α=0.016, β=0.0012, target CBR 0.68, 200 ms updates.
This is security-relevant: DCC-induced rate drops change misbehavior-evidence density.

**8. `rssi_dbm` observable** — added to evidence rows. It is legitimately receiver-measurable, so
MA-visible, but must be computed from **true** geometry on the channel side; Sybil ghosts inherit
the attacker's true-position RSSI, which is exactly what makes an RSSI-vs-claimed-distance detector
possible. Requires an explicit leakage-linter assertion.

## Building footprints are already on disk (verified 2026-08-30)

The NLOSb building-blockage model needs no new network dependency, no new Overpass query and no new
cache. `osm.py:49` fetches `https://overpass-api.de/api/map?bbox=`, which returns **all** raw OSM
data in the bounding box; the importer then keeps only `highway` ways and discards everything else.
The buildings are therefore already being downloaded and cached today.

Measured on the existing cache (`datasets/_osmcache/osm_816d9f25303fea7e.xml`, 16.5 MB, Ingolstadt):

| Property | Value |
|---|---|
| Building ways with **complete** geometry | **1482** (0 incomplete) |
| Closed rings | 1482 / 1482 |
| Nodes available for resolution | 22,248 |
| Carrying `height` or `building:levels` | 208 (14%) |
| Dominant kinds | `yes` 1141, `house` 139, `apartments` 63 |

Overpass itself is reachable from this host (HTTP 200, rate limit 2, slots available), so re-fetching
other cities works — but the Ingolstadt data needed for the InTAS scenarios is already local.

Only 14% of footprints carry height, so the NLOSb model must default a building height. That is
acceptable: the 3GPP TR 37.885 urban-NLOS formula is a function of distance only, and the
LOS/NLOSb decision is a 2-D segment-vs-polygon blockage test. Height matters only if the optional
per-wall Sommer attenuation variant is used.

### Projection alignment — the trap to avoid

`osm.py:107-116` builds a **local equirectangular projection** whose origin is
`lat0, lon0 = min(lats), min(lons)` and whose scale is
`kx = 111320·cos(mean_lat)`, `ky = 111320` — and those `lats`/`lons` are gathered **from the road
ways only**. Projecting buildings with an independently-derived origin would misalign them against
the road graph, silently corrupting every LOS/NLOS classification while still producing
plausible-looking output.

So `osm_to_network` must return (or persist alongside the network JSON) the exact tuple
`(lat0, lon0, kx, ky)`, and the building extraction must reuse it verbatim. Add an assertion that
projected building centroids fall inside the road network's bounding box.

## Reference curves to pin as refdata

- 90% cooperative awareness up to ~200 m urban, >500 m highway (Boban & d'Orey 2015).
- PDR ≥ 50% defines "effective range" (Safety Pilot Model Deployment, Ann Arbor).
- PDR-vs-RSSI empirical curve, FLOURISH Bristol ITS-G5 (DOI 10.5523/bris.eupowp7h3jl525yxhm3521f57).
- Urban V2I max range ≈700 m at frame-success-ratio 0.25.
- Gray zone: PDR falling 90%→20% must span ≥100 m (a step-function model fails this by construction).

## Verified code anchors (read 2026-08-30, `run.py`)

Each defect below was confirmed by reading the reception loop directly — do not re-derive.
**Line numbers were re-verified on 2026-08-30 after ADR 0002** (`true_speed`/`true_heading` in the
ground-truth emission record) shifted everything below `run.py:2611` down by 13 lines; the code at
each anchor is unchanged.

- **`run.py:2755-2756`** — shadowing draw is
  `random.Random(f"{cfg.seed}:shadow:{b['digest']}:{rx.vid}:{step}")`. Two bugs in one line: keyed
  on the **cert digest** (so pseudonym rotation resamples the channel) and on **`step`** (so it is
  i.i.d. per step, with no spatial correlation). Replace with an AR(1) process carried per link and
  keyed on the true vehicle id.
- **`run.py:2763`** — `loss = packet_loss_base + nlos_loss*(dist/rr) + cong + wx_loss`. Additive;
  can exceed 1.0. Replace with the independent-survival product.
- **`run.py:2760`** — congestion is `min(0.8, max(0, (load−chan_capacity)/chan_capacity) * 0.5)`,
  a linear ramp on raw message count. Replace with modeled CBR.
- **`run.py:2764`** — the packet-loss draw uses the **global** `rng`, not a keyed stream. This sits
  *outside* the per-model branch, so under `radio_model="geometric"` any change in the size of
  `in_range` shifts global RNG consumption. That is safe for the golden digest only because
  geometric is opt-in and the default `disc` path is untouched — but it means the geometric branch
  should move its own draws onto keyed streams rather than lean on `rng`.
- **`run.py:2706`, `2718-2758`** — `radio_logdist` is the existing precedent for adding a radio
  model: a boolean selected from `cfg.radio_model`, with the default `"disc"` path taking none of
  the branch and drawing no extra RNG. Model the `"geometric"` branch on exactly this shape.
- **`run.py:2698-2709`** — the broadcast spatial hash (cell = radio range). Reuse this structure for
  the building/vehicle occlusion index rather than introducing a geometry library.
- **`run.py:2695`** — receivers are vehicles + RSUs, with RSUs appended last specifically to keep
  vehicle-side RNG draws unchanged. Preserve that ordering discipline.

## Java-side parity

Same model family in `ScmsBeaconApp`: parse `buildings.poly.xml` (InTAS ships 5.7 MB of real
footprints, currently unused), fix the claimed-position NLOS bug, add DCC, and unify the weather-loss
table with Python's.
