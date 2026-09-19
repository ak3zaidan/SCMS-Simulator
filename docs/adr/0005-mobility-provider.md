# ADR 0005 — Mobility provider: native engine plus SUMO co-simulation behind one interface

- **Status:** Proposed (2026-09-18)
- **Related:** `03-interfaces.md` §3, `04-models.md` §2, `02-architecture.md` §7. Evidence: R8 §A.2, R10 §A–B.

## Context

The brief asks whether SUMO (EPL-2.0) through TraCI or libsumo should be the high-fidelity mobility tier behind the `Mobility` interface, with a native engine as the built-in tier, weighing determinism, step-time overhead, OSM import maturity, pedestrian support, and deployment friction.

## Decision

**Both, behind one `Mobility` interface.**

1. **Native engine** (`v2xw-mobility`) provides the `abstract` tier (kinematic along lanes) and the `medium` tier (IDM/MOBIL/gap acceptance/signals/weather, ported from the legacy formulas with cited parameters, 04-models §2). It is the default, requires no external install, runs in WASM, and is fully under our determinism rules.
2. **SUMO co-simulation** (`v2xw-mobility-sumo`) provides the `high` tier: Krauss/IDM/Wiedemann car-following, LC2013 lane changes, SUMO's junction model, and pedestrian models (striping, JuPedSim) [R8 §A.2]. Integration is **out of process over TraCI** by default (isolation, GUI support, multiple clients), with **libsumo** as an in-process option when installed (same API, no socket overhead, one simulation per process, no GUI on Windows) [R8 §A.2: sumo.dlr.de/docs/Libsumo]. SUMO is never vendored; the manifest records its version and seed.
3. **Import path.** SUMO's `netconvert` is also the converter from OpenDRIVE and for complex OSM cases (`--osm.sidewalks`, `--osm.crossings`, `--osm.turn-lanes`, `--tls.guess-signals`, `--junctions.join` [R8 §A.2; R10 §A12]) into the canonical `world-1` format; the native OSM importer handles the no-SUMO case.
4. **Installation.** SUMO is pip-installable (`eclipse-sumo`, `sumolib`, `traci`, `libsumo` at 1.27.1 on PyPI) and packaged in Debian/Ubuntu and a project PPA; there is **no Homebrew formula, no Chocolatey package, and the conda-forge `sumo` is an unrelated materials-science tool** [R8 §C]. `just sumo-install` therefore uses the PyPI wheels first and documents apt/PPA and the official Windows installer as alternatives.

## Determinism rules for the SUMO tier

SUMO documents that runs with the same version, arguments and inputs are expected to reproduce across Windows, Linux and macOS, with a Mersenne Twister seeded 23423 by default and decoupled RNG instances per aspect; documented breakers are `--device.rerouting.threads` with `--weights.random-factor`, Proj coordinate-transform differences across platforms, and some models' logging paths (EIDM, DriverState, possibly Wiedemann/ToC) [R8 §A.2: sumo.dlr.de/docs/Simulation/Randomness]. The adapter therefore: pins `--seed`, forbids rerouting threads, projects coordinates in our engine (SUMO receives a plain x/y network), disables the affected devices unless the scenario opts in with a manifest warning, and drives SUMO with a fixed step equal to `Δt_mob`. The golden test for the SUMO tier is per SUMO version.

## Alternatives

| Option | Determinism | Fidelity | Deployment friction | Browser | Verdict |
|---|---|---|---|---|---|
| A. Native only | best | medium (no Wiedemann/pedestrian models initially) | none | yes | insufficient for traffic-engineering studies |
| B. SUMO only (TraCI) | good with the rules above | high | external install; no browser | no | blocks Phase 1 and WASM |
| C. Both behind one interface (chosen) | best/good | both | optional install | yes (native) | chosen |
| D. MOSAIC as mobility federate | good | high (via SUMO) | Java + MOSAIC RTI | no | rejected: adds a runtime with no gain over direct TraCI |
| E. CARLA co-simulation | unknown | high physics | GPU, Unreal Engine | no | rejected for mobility; noted for 3D asset ideas (MIT code, CC-BY assets [R8 §A.1]) |

## Consequences

- The `Mobility` interface must expose lane-level kinematics identically for both tiers (03-interfaces I-M4); the SUMO adapter maps SUMO lane ids to `world-1` lane ids at import.
- Step overhead of SUMO per vehicle is not published [R8 §A.2]; Phase 3 measures it and records it in the model card.
- The legacy `mapgen.py` recipes (netconvert/netgenerate/randomTrips options) are reused as documented presets (01-inventory §3.7).
