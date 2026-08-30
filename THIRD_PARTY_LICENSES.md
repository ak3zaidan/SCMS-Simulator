# Third-party code and licences

This project is Apache-2.0 (see `LICENSE`). A small amount of third-party code is **ported into**
this tree and keeps its original licence; everything else is used as an external tool or as a git
submodule and is not redistributed here.

## Ported source (in this repository, under its own licence)

| File | Licence | Origin |
| --- | --- | --- |
| `scms-sim/mosaic-apps/scms-app/src/main/java/org/scms/realism/DriverProfile.java` | **EPL-2.0** | VeReMi-NextGen, `Generator/simulation/mosaic/applications/CamApp/src/main/java/entities/DriverProfile.java`, itself derived from the Eclipse MOSAIC example applications — Copyright (c) 2020 Fraunhofer FOKUS and others |
| `scms-sim/mosaic-apps/scms-app/src/main/java/org/scms/realism/SensorErrorModel.java` | **EPL-2.0** | VeReMi-NextGen, `Generator/simulation/mosaic/applications/CamApp/src/main/java/util/SensorErrorModel.java`, same MOSAIC/Fraunhofer FOKUS lineage |

Both files carry a full SPDX header, the upstream path, the Fraunhofer FOKUS copyright notice, and
an explicit, itemised list of the behavioural modifications made here (determinism keying,
simulation-time instead of wall-clock, and the decision **not** to transmit the realised sensor-error
vector in the CAM — upstream does, which would let a receiver subtract it and recover the true
position, breaching this project's ground-truth firewall). Eclipse Public License 2.0:
<https://www.eclipse.org/legal/epl-2.0/>.

No other file in `scms-sim/mosaic-apps/` is derived from third-party source: `ScmsBeaconApp`,
`ScmsRsuApp`, `CamDetector`, `SignedCam`, `ScmsBackend`, `AttackLib`, `Scms`, and `LinkageEngine`
are original to this project (Apache-2.0) and only call published MOSAIC APIs.

## Used, not redistributed

| Component | Licence | How it is used |
| --- | --- | --- |
| Eclipse MOSAIC 25.2 | EPL-2.0 | External runtime (`MOSAIC_HOME`); our application jar is built against its published APIs and dropped into a scenario. Not vendored. |
| Eclipse SUMO 1.25.0 | EPL-2.0 | External simulator (`SUMO_HOME`), invoked as a process (`sumo`, `netgenerate`, `netconvert`, `randomTrips.py`); `sumolib` is imported from `SUMO_HOME/tools` at call time. Not vendored, not a pip dependency. |
| VeReMi-NextGen | EPL-2.0 | Git submodule at `third_party/veremi-nextgen` (its own `LICENSE` / `THIRD_PARTY_LICENSES.md` apply). Supplies the InTAS Ingolstadt scenario, its calibrated demand and its detector loops. |
| InTAS (Ingolstadt Traffic Scenario) | see the submodule | Consumed through the VeReMi-NextGen submodule; its route files are directory-junctioned into generated scenarios, never copied into this repository. |
| OpenStreetMap data | ODbL 1.0 | Fetched on demand via Overpass for `osm_*` map keys and cached under `scms-sim/scenarios/_mapcache`. © OpenStreetMap contributors. Derived networks inherit ODbL obligations. |
| `cryptography`, `pydantic`, `numpy`, `pandas`, `pytest` | Apache-2.0 / MIT / BSD | Ordinary pip dependencies, not vendored. |

## Reference data

`src/scms_sim_ref/datagen/refdata/*.json` contains **numbers transcribed from published literature
and standards** (3GPP TR 37.885, ETSI TS 102 687 / EN 302 637-2, FHWA traffic-analysis criteria,
and measurement studies), not third-party source code. Every entry carries a `source` citation, a
short `cite` label, and a `confidence` field; values that were derived rather than transcribed carry
an explicit `derivation`. Where a corpus (e.g. highD) is licence-gated and its percentile tables
could not be reproduced here, the entry is an explicit `available: false` placeholder rather than an
invented number.
