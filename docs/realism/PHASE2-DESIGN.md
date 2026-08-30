# Phase 2 design note — V2X channel realism

Binding decisions for the Phase 2 implementation agents, taken after reading
[investigation/research-net-sota.json](investigation/research-net-sota.json) and probing the host.
These narrow the roadmap's §3.2 sketch into an implementable contract.

## Host-derived constraints (verified 2026-08-30)

| Fact | Consequence |
|---|---|
| Installed: numpy 2.5.2, pandas 3.0.5, pyarrow 25.0.1, libsumo/traci/sumolib **1.25.0** (exact match to the SUMO binary) | Phase 3 mobility has a working substrate; no DLL-mismatch fix needed |
| **No** shapely, **no** scipy | Geometry must be pure numpy: segment-vs-polygon tests over a uniform spatial grid, mirroring the existing spatial hash at `run.py:2689`. Do **not** add a geometry dependency |
| `requirements.txt` is only cryptography/pydantic/pytest | numpy is already a de-facto runtime dep of `datagen`; keep `mock_pipeline` importable without new deps — guard optional imports |
| WSL feature **Disabled**, no distros; enabling needs a reboot | **No in-loop or offline ns-3 / ms-van3t / VaN3Twin federation.** MOSAIC's native-Windows SNS is *less* realistic than what we already have, so it is not adopted either |

Because we cannot run a network simulator to generate golden curves, published measurement and
analytical-model curves are pinned as reference data instead. The literature supports this: the
closed-form models below were themselves validated against full network simulators over wide
parameter ranges.

## The model stack (opt-in `radio_model="geometric"`)

Applied at the single reception choke point (`run.py:2673-2752`). Every stage is opt-in and drawn
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

## Reference curves to pin as refdata

- 90% cooperative awareness up to ~200 m urban, >500 m highway (Boban & d'Orey 2015).
- PDR ≥ 50% defines "effective range" (Safety Pilot Model Deployment, Ann Arbor).
- PDR-vs-RSSI empirical curve, FLOURISH Bristol ITS-G5 (DOI 10.5523/bris.eupowp7h3jl525yxhm3521f57).
- Urban V2I max range ≈700 m at frame-success-ratio 0.25.
- Gray zone: PDR falling 90%→20% must span ≥100 m (a step-function model fails this by construction).

## Java-side parity

Same model family in `ScmsBeaconApp`: parse `buildings.poly.xml` (InTAS ships 5.7 MB of real
footprints, currently unused), fix the claimed-position NLOS bug, add DCC, and unify the weather-loss
table with Python's.
