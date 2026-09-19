# 04 — Model catalog

Status: design draft for review (2026-09-18). Companion to `02-architecture.md` (fidelity ladder, §7) and `03-interfaces.md` (the traits every model implements; model-card schema §12). Section 3 (propagation, fading, obstacles, weather attenuation, antennas, GNSS, clock) is written from research sheet R3 and §13 (validation plan) from R1, R2, R2d, R3, R5, R7 and R10; both were placeholders in the first draft.

Every number in this document comes from a research sheet (R1, R2, R2b, R2c, R2d, R2e, R4, R5, R10, R11) or from the frozen legacy reference (`legacy/scms_sim_ref/`, today `src/scms_sim_ref/`). A value the sheets mark UNVERIFIED keeps that tag here. A parameter with no sourced value is written `TODO: calibrate` with a one-line plan naming the measurement or document that would settle it. Nothing is invented.

### How to read a model entry

Each model is an entry with these fields, which map one to one onto the model-card schema (03-interfaces §12):

| Field | Meaning |
|---|---|
| Model id | registry name, `family/subfamily/name`, lowercase, no dots (so IEEE 802.11p becomes `80211p`) |
| Family | one of the card families (`world`, `mobility`, `vru`, `weather`, `gnss`, `clock`, `propagation`, `fading`, `obstacle`, `phy`, `mac`, `dcc`, `net`, `fragmenter`, `backhaul`, `cellular`, `backend-net`, `codec`, `generator`, `envelope`, `primitive`, `crypto-backend`, `verification-policy`, `safety-app`, `service-model`, `perception`, `attacker`, `detector`, ...) |
| Interface | the trait from 03-interfaces the model implements |
| Tiers | which of `abstract`, `medium`, `high` the model serves; a model may serve one tier only |
| Purpose | one paragraph |
| Equations or algorithm | the actual rule, not a summary |
| Parameters | name, unit, default, source; the source is one of `standard` (clause), `paper` (author year, section), `datasheet`, `dataset`, `code (legacy)` (the frozen reference, file and line), or `todo-calibrate` (with a plan) |
| Assumptions and limitations | what the model takes for granted and where it is known to be wrong |
| Ignores | what this tier leaves out relative to the next tier up (for the top tier: relative to reality) |
| Validation | `unvalidated`, `unit-tested`, `literature-checked`, `field-checked`, with the reference curve it is checked against |

Citations are inline in the form [Author Year §x] or [Standard clause]; the reference list at the end collects URLs. Status tags: **UNVERIFIED** (the sheet could not confirm the value against a primary source), **secondary** (value read from a paper that reproduces a paywalled standard), **DERIVED** (arithmetic on cited values, shown), **model output** (computed from a cited model, not measured).

### Section numbering

The other design documents cite this catalog by its final section numbers (§4.9 abstract-tier calibration, §7.3 fragmenters, §8.4 size-model tolerances, §9.4 primitives, §11 safety applications, §13 validation plan, §14 detector thresholds, §3.8 GNSS and clock).

### Index of model ids

| Model id | Family | Interface | Tiers | Section |
|---|---|---|---|---|
| `world/format/world-1` | world | `World` | all | 1.1 |
| `world/source/osm` | world | `WorldSource` | all | 1.2 |
| `world/source/sumo-net` | world | `WorldSource` | all | 1.2 |
| `world/source/opendrive` | world | `WorldSource` | all | 1.2 |
| `world/source/procedural-grid`, `-radial`, `-suburban`, `-highway`, `-mixed` | world | `WorldSource` | all | 1.2 |
| `world/source/json-legacy` | world | `WorldSource` | all | 1.2 |
| `world/buildings/osm`, `world/buildings/overture`, `world/buildings/ms-footprints`, `world/buildings/google-open` | world | `WorldSource` (layer) | all | 1.3 |
| `world/terrain/srtm-30`, `world/terrain/copernicus-glo-30` | world | `WorldSource` (layer) | all | 1.4 |
| `mobility/kinematic/lane-follow` | mobility | `Mobility` | abstract | 2.1 |
| `mobility/car-following/idm` | mobility | `CarFollowing` | medium | 2.1 |
| `mobility/car-following/krauss` | mobility | `CarFollowing` | medium (native port), high (SUMO) | 2.1 |
| `mobility/car-following/wiedemann-99` | mobility | `CarFollowing` | medium | 2.1 |
| `mobility/car-following/gipps` | mobility | `CarFollowing` | medium | 2.1 |
| `mobility/lane-change/mobil` | mobility | `LaneChange` | medium | 2.2 |
| `mobility/lane-change/lc2013` | mobility | `LaneChange` | high (SUMO) | 2.2 |
| `mobility/intersection/gap-acceptance-hcm` | mobility | `IntersectionControl` | medium | 2.3 |
| `mobility/intersection/signal-fixed-time` | mobility | `IntersectionControl` | medium | 2.3 |
| `mobility/intersection/roundabout-fhwa` | mobility | `IntersectionControl` | medium | 2.3 |
| `mobility/intersection/two-coloring-legacy` | mobility | `IntersectionControl` | abstract | 2.3 |
| `mobility/routing/dijkstra`, `mobility/routing/dynamic-reroute` | mobility | `Router` | all | 2.4 |
| `mobility/demand/poisson-thinned`, `mobility/demand/od-gravity`, `mobility/demand/tr36885-drop` | mobility | `Demand` | all | 2.4 |
| `vru/pedestrian/social-force`, `vru/pedestrian/striping-sumo`, `vru/cyclist/lane-follow` | vru | `VruMobility` | medium/high | 2.5 |
| `weather/driving/fhwa-table`, `weather/driving/legacy-multipliers` | weather | `WeatherModel` | all | 2.6 |
| `mobility/classes/sumo-vtypes`, `mobility/classes/legacy-fleet`, `mobility/classes/tr37885-types` | mobility | (data) | all | 2.7 |
| `mobility/sumo/traci-cosim` | mobility | `Mobility` | high | 2.8 |
| `gnss/error/ou-bias-legacy`, `clock/drift/none` | gnss, clock | `GnssModel`, `ClockModel` | all | 2.10 |
| `propagation/free-space`, `propagation/two-ray-ground` | propagation | `Propagation` | all | 3.1 |
| `propagation/log-distance-shadowing` (presets `abbas-*`, `kunisch-*`, `cheng-*`, `karedal-*`) | propagation | `Propagation` | medium, high | 3.2 |
| `propagation/tr37885`, `propagation/winner-plus-b1` | propagation | `Propagation` + `ObstacleModel` | medium, high | 3.3 |
| `fading/nakagami-m`, `fading/none` | fading | `Fading` | high; medium | 3.4 |
| `obstacle/building/sommer-2011`, `obstacle/terrain/knife-edge-p526`, `obstacle/vehicle/tr37885-nlosv`, `obstacle/vehicle/knife-edge-boban`, `obstacle/foliage/boban-mel` | obstacle | `ObstacleModel` | medium (building, NLOSv), high (all) | 3.5 |
| `weather/attenuation/itu-r` | weather | `Propagation` term | high | 3.6 |
| `propagation/antenna/isotropic-gain`, `propagation/antenna/pattern`, `phy/receiver/noise-sensitivity` | propagation, phy | `Propagation`, `Phy` | all; high; all | 3.7 |
| `gnss/error/gauss-markov`, `clock/drift/tcxo-ocxo` | gnss, clock | `GnssModel`, `ClockModel` | all; medium, high | 3.8 |
| `phy/80211p/ofdm-10mhz` | phy | `Phy` | medium, high | 4 |
| `phy/80211p/nist-per` | phy | `Phy` (error model) | medium, high | 4.6 |
| `mac/80211p/edca-ocb` | mac | `Mac` | high | 4.3 |
| `mac/80211p/slotted-abstraction` | mac | `Mac` | medium | 4.8 |
| `phy/abstract/distance-load-table` | phy | `Phy` | abstract | 4.9 |
| `phy/abstract/disc-legacy`, `phy/abstract/logdistance-legacy` | phy | `Phy` | abstract | 4.9 |
| `phy/lte-v2x/mode4`, `mac/lte-v2x/sps-sensing` | phy, mac | `Phy`, `Mac` | medium, high | 5.1 |
| `phy/nr-v2x/mode2`, `mac/nr-v2x/sps-sensing` | phy, mac | `Phy`, `Mac` | medium, high | 5.2 |
| `phy/lte-v2x/bler-lut-r1-160284` | phy | `Phy` (error model) | medium, high | 5.1 |
| `dcc/etsi/adaptive-ts102687`, `dcc/etsi/reactive-ts102687`, `dcc/etsi/cross-ts103175` | dcc | `Dcc` | all | 6.1-6.3 |
| `dcc/sae/j2945-1-rate-power` | dcc | `Dcc` | all | 6.4 |
| `net/wsmp/1609-3`, `net/gn-btp/en302636` | net | `NetLayer` | all | 7.1-7.2 |
| `fragmenter/none`, `fragmenter/facilities-segmentation`, `fragmenter/cert-cycle-partial-hybrid`, `fragmenter/generic-sdu` | fragmenter | `Fragmenter` | abstract/medium/high | 7.3 |
| `generator/bsm-j2945-1`, `generator/cam-en302637-2`, `generator/denm-en302637-3`, `generator/spat-map`, `generator/vam-ts103300-3`, `generator/cpm-ts103324`, `generator/wsa-1609-3`, `generator/psm-j2945-9` | generator | `MessageGenerator` | all | 8.1 |
| `codec/uper/rasn-etsi`, `codec/coer/rasn-1609-2`, `codec/size-model/j2735` | codec | `MessageCodec` | all | 8.3 |
| `envelope/ieee-1609-2`, `envelope/etsi-ts103097` | envelope | `SecurityEnvelope` | all | 9.1 |
| `primitive/ecdsa-p256-sha256`, `primitive/ecdsa-brainpoolp256r1`, `primitive/ecdsa-p384`, `primitive/ecqv-p256`, `primitive/ml-dsa-44`, `-65`, `-87`, `primitive/falcon-512`, `-1024`, `primitive/slh-dsa-sha2-128s`, `-128f`, `primitive/ml-kem-512`, `-768`, `-1024`, `primitive/hybrid-mldsa44-ecdsa-p256`, `primitive/hybrid-falcon512-ecdsa-p256` | primitive | `Primitive` | all | 9.4 |
| `crypto-backend/modeled`, `crypto-backend/real` | crypto-backend | `CryptoBackend` | modeled, real | 9.6 |
| `cellular/uu/fixed-latency`, `cellular/uu/cell-capacity-mm1`, `cellular/uu/handover-outage` | cellular | `CellularUu` | abstract/medium/high | 10.1 |
| `backhaul/fixed`, `backhaul/measured-tn` | backhaul | `Backhaul` | abstract, medium+ | 10.2 |
| `service-model/fixed-latency`, `service-model/mmc-batching`, `service-model/measured-availability` | service-model | `ServiceModel` | abstract/medium/high | 10.3 |
| `safety-app/fcw-vsca`, `safety-app/eebl-vsca`, `safety-app/ima-vsca`, `safety-app/bsw-lcw-vsca`, `safety-app/dnpw-vsca`, `safety-app/vru-warning` | safety-app | `SafetyApp` | all | 11 |
| `metric/ssam/ttc-pet-drac` | metric | `MetricProvider` | all | 11 |
| `perception/disc-sensor`, `perception/occluded-sensor`, `perception/cpm-quality-ts103324` | perception | `Perception` | medium, high | 12.1-12.2 |
| `attacker/jammer/constant`, `attacker/jammer/reactive`, `attacker/jammer/constant-pilot` | attacker | `Attacker` | medium, high | 12.3 |
| `detector/f2md-checks`, `detector/ts103759-observations`, `detector/legacy-12` | detector | `Detector` | all | 14 |

## 1. World model

### 1.1 Canonical lane-level format decision

**Model id** `world/format/world-1`. Family `world`. Holds the `World` struct of 03-interfaces §2: origin, bbox, `RoadNetwork` (lanes with centerline polylines carrying z, width, speed limit, allowed classes, lane type; junctions with internal lanes, conflict matrix and control type; connections; crossings), buildings (footprint, height, material class, LOD hints), terrain DEM, signal plans, RSU and cell sites, land use zones, provenance, content hash.

Candidates compared (data from [R10 §A15]; the last four columns are this project's additional requirements):

| Format | License | Lane connectivity | Signals | Elevation | Sidewalks, crossings | Buildings | Tooling | Terrain DEM | Propagation environment class | Provenance and license record | Render hints |
|---|---|---|---|---|---|---|---|---|---|---|---|
| SUMO `net.xml` | EPL-2.0 project; the format has no separate license (SUMO project license EPL-2.0 is UNVERIFIED as exact wording, [R10 §A12]) | explicit `<connection>` with internal lanes (`<junction intLanes>`) | `<tlLogic>` phases keyed by `linkIndex` | z in lane `shape`; `--heightmap.*` import | first class: sidewalk lanes, `crossing` and `walkingarea` functions | not in the network (separate `.poly.xml`) | netconvert, netedit, duarouter, TraCI, large Python toolset | no | no | no | no |
| ASAM OpenDRIVE | free to use under ASAM terms (not an OSI license) | lane linkage plus junction connecting roads | signals as positioned objects with timing reference | elevation profiles on the reference line | roadmarks and objects only; no pedestrian network model | no | CARLA, esmini, VTD, dSPACE, netconvert import and export | no | no | no | no |
| Lanelet2 | BSD-3-Clause | native routing graph, regulatory elements | `lanelet2_traffic_rules` | 2D and 3D native | lanelets by subtype | no | Autoware ecosystem | no | no | no | no |
| osm2streets | UNVERIFIED (README does not state; parent A/B Street is Apache-2.0, not re-confirmed) | roads as lane lists left to right (type, direction, width); turning movements planned, not implemented | not implemented (planned) | not addressed | snaps footways and cycletracks; crossings planned | no | JS/WASM and Python bindings, StreetExplorer | no | no | no | no |
| Own (`world-1`) | Apache-2.0 (this repository) | designed in | designed in | designed in | designed in | designed in | must be built | designed in | designed in | designed in | designed in |

**Decision** (recorded in 02-architecture §4): the canonical format is our own `world-1` (FlatBuffers schema, versioned), because no external format carries buildings, terrain, propagation environment classes, provenance and render hints in one object, and the engine needs one object for mobility, propagation and rendering. Import is lossless from SUMO `net.xml` and from OpenDRIVE via SUMO `netconvert` (`--opendrive-files`, [netconvert options in R10 §A12]) when SUMO is installed, and a native OSM importer with osm2streets-style lane inference runs when it is not. SUMO `net.xml` is the closest external match and remains the interchange format for the SUMO co-simulation tier (§2.8).

What each import loses (design statement; the importer writes the corresponding `WorldProvenance.transformations` entries, invariant I-W3):

| Import path | Preserved | Lost or defaulted |
|---|---|---|
| SUMO `net.xml` | edges, lanes, internal lanes, connections, `tlLogic` programs, `request` conflict matrices, sidewalks, crossings, walking areas, lane z if present | buildings (none in the format), land use, terrain beyond lane z, material classes; environment class is defaulted from lane density (`TODO: calibrate`, plan: compare defaulted class to hand-labeled classes on the Phase 2 scenarios) |
| OpenDRIVE via netconvert | road reference line, lanes, junctions, elevation profile, signals as objects | pedestrian network unless present as lanes; signal timing when only a reference exists; everything netconvert itself drops (its conversion log is copied into provenance) |
| OSM native | drivable ways, lane count, one-way, turn lanes, sidewalks, crossings, traffic signal nodes, building footprints and tagged heights | signal plans (only signal presence; the plan is generated by `mobility/intersection/signal-fixed-time` defaults), turning movements not encoded in tags (inferred geometrically), heights where untagged (§1.3 default rule) |
| Procedural | everything the generator defines | nothing; provenance records the generator id and seed |
| Legacy JSON | point nodes and undirected edges with optional per-edge speed | lanes (defaulted to one per direction), directions (both), internal lanes (generated as straight connectors), signals (legacy 2-coloring, §2.3) |

### 1.2 Importers

**`world/source/osm`** (`WorldSource`). Reads an OSM XML extract for a bbox. Tags read: `highway` (class, used for default speed and lane count), `maxspeed`, `oneway`, `lanes`, `lanes:forward`, `lanes:backward`, `turn:lanes`, `junction`, `restriction` relations, `traffic_signals` nodes, `sidewalk`, `crossing`, `cycleway`, and the building tags of §1.3. The lane inference follows the osm2streets schema (a road is a list of lanes left to right with type, direction and width; intersections are polygon areas) and its simplifications: collapse unnecessary two-road intersections, merge dual-carriageway "sausage links", merge dog-leg intersections, snap parallel cycletracks and footways to the main road [osm2streets README, R10 §A14]. Where SUMO is present the importer may delegate to netconvert with `--osm.sidewalks`, `--osm.crossings`, `--osm.turn-lanes`, `--osm.lane-access`, `--osm.bike-access`, `--osm.elevation`, `--junctions.join` (join distance default 10 m), `--tls.guess-signals` (distance default 25 m) [netconvert options, R10 §A12 cache].

Overpass limits the importer must respect [OSM Wiki Overpass API, R10 §A2]: casual public use below 10,000 queries/day and 1 GB/day; a "regular application" about 1/100th of that; default query timeout 3 min (extendable to 900 s); on HTTP 429 or 406 back off at least 30 s, send an identifying `User-Agent`, do not parallelize; commercial or bulk use needs self-hosting or a paid provider. The legacy fetcher (`osm.py`, `_OVERPASS = https://overpass-api.de/api/map?bbox=`) cached raw XML by bbox hash and capped bboxes at 0.05 × 0.04 degrees and 380 nodes with RDP simplification at 10 m tolerance, escalating tolerance and dropping minor classes until the cap held [`osm.py` L85-190, code (legacy)]; the node cap is removed and the 11 city bboxes are kept as presets (01-inventory §3.4). Projection: the legacy equirectangular scale `kx = 111320 · cos(lat_mid)` [`osm.py` L111] is replaced by a local tangent plane recorded in `GeoOrigin`.

ODbL: an OSM-derived world is very likely a Derivative Database, not a Produced Work; export obligations are handled in `08-measurement-and-data.md` §9 [OSMF Produced Work guideline, R10 §A1].

**`world/source/sumo-net`** and **`world/source/opendrive`**. Parse `net.xml` elements `<edge>`, `<lane>` (index 0 rightmost, `speed` m/s, `length`, `shape`), `<junction>` (`incLanes`, `intLanes`, `shape`), `<connection>` (`from`, `to`, `fromLane`, `toLane`, `via`, `dir`, `state`, `linkIndex`), `<tlLogic>` (`<phase duration state>`), `<request>` (`response`, `foes`, `cont`) [SUMO Road Networks docs, R10 §A12]. OpenDRIVE goes through netconvert (`--opendrive-files`); netconvert import formats: plain XML, OSM, VISUM, Vissim, OpenDRIVE, MATSim, SUMO, Shapefile, RoboCup, DlrNavteq/GDF [R10 §A12].

**Procedural generators** (`WorldSource`, deterministic from a seed):

| Model id | Geometry | Parameters (unit, default, source) |
|---|---|---|
| `world/source/procedural-grid` | Manhattan grid | preset `tr36885-urban`: block 433 m × 250 m, 2 lanes per direction, lane width 3.5 m, sidewalk 3 m, minimum area 1,299 m × 750 m, antenna height 1.5 m [TR 36.885 Table A.1.2-1, R2c]; street width 20 m [TR 37.885 Annex A Fig. A-2, R2c]; preset `legacy`: `grid_w` 6, `grid_h` 6, `grid_block_m` 120, `grid_dropout` 0, `n_lanes` 1, `lane_width_m` 3.5 [`run.py` L412-424, code (legacy)] |
| `world/source/procedural-radial` | ring roads plus radial arms (legacy `spider_graph(arms, rings, block)`, ring circumference `n · block`) | arms, rings, block from legacy `grid_w`, `grid_h`, `grid_block_m` [`roads.py` L311-322, L683-694, code (legacy)]; irregularity via `grid_dropout` fraction of removed edges |
| `world/source/procedural-suburban` | cul-de-sac tree | `TODO: calibrate` (plan: derive block and cul-de-sac length distributions from three OSM suburban bboxes and record them as the preset) |
| `world/source/procedural-highway` | straight or looped freeway with interchanges | preset `tr36885-freeway`: 3 lanes per direction, lane width 4 m [TR 36.885 Table A.1.2-1, R2c]; preset `todisco`: 2 km, 3 lanes per direction, wrap-around [Todisco 2021, R2d]; interchange ramp geometry `TODO: calibrate` (plan: take ramp lengths from one OpenDRIVE sample and one OSM motorway junction) |
| `world/source/procedural-mixed` | grid core, radial arterials, highway ring | composition of the above; parameters inherited |

**`world/source/json-legacy`**. Reads `{"nodes":[[x,y]], "edges":[[a,b,speed?]]}` [`roads.py` `CustomNetwork`, 01-inventory §3.4] and emits one lane per direction with legacy defaults.

### 1.3 Buildings and heights

| Model id | Source data | License | Height information | Notes |
|---|---|---|---|---|
| `world/buildings/osm` | OSM `building` ways and relations | ODbL | `height` (ground contact to roof top, excluding antennas), `building:levels` (above-ground floors, supplements `height`), `min_height`, `building:min_level` [OSM Simple 3D Buildings, R10 §A4] | tag coverage is sparse and unquantified (UNVERIFIED, [R10 §A4]) |
| `world/buildings/overture` | Overture Buildings theme | ODbL for OSM-derived themes ("© OpenStreetMap contributors, Overture Maps Foundation") [Overture attribution, R10 §A5] | presence of a height attribute UNVERIFIED (schema reference not opened, [R10 §A5]) | |
| `world/buildings/ms-footprints` | Microsoft Global ML Building Footprints | CDLA Permissive 2.0 | about 174 million of about 1.4 billion footprints carry a neural-network height estimate in meters; `-1` means no estimate; confidence 0-1; false-positive rate about 0.1-2.2 % by region [Microsoft GlobalMLBuildingFootprints README, R10 §A6] | line-delimited GeoJSON, EPSG:4326 |
| `world/buildings/google-open` | Google Open Buildings | CC-BY-4.0 or ODbL at the user's choice | none (footprint only) [Google Open Buildings, R10 §A7] | Africa, South and Southeast Asia, Latin America; confidence 0.65-1.0 |

Height defaulting rule (parameter `building_height_default`): when a footprint has `building:levels` but no `height`, height = levels × `meters_per_level`; when neither exists, height = `default_height_m` by land use. Both `meters_per_level` and `default_height_m` are `TODO: calibrate`: the OSM wiki page consulted states no standard level-to-meter factor [R10 §A4]; plan: fit `meters_per_level` and per-land-use defaults by regressing Microsoft-estimated heights on OSM `building:levels` for the Phase 2 city bboxes, and record the fit and its residual in the card. Provenance records the rule applied per building (I-W3). Material class (for §3 obstacle models) defaults to `unknown` and is set only from explicit tags.

### 1.4 Terrain

| Model id | Data | Resolution | License and attribution |
|---|---|---|---|
| `world/terrain/srtm-30` | NASA SRTMGL1 | 30 m × 30 m (about 1 arc-second) | openly shared without restriction under EOSDIS data-use guidance; citation requested, not legally required [NASA Earthdata SRTMGL1, R10 §A8]; vertical accuracy UNVERIFIED (page defers to Rodriguez et al. 2006) |
| `world/terrain/copernicus-glo-30` | Copernicus DEM COP-DEM-GLO-30-F | 30 m | free of charge; rights: reproduction, distribution, communication to the public, adaptation, modification and combination, worldwide, unlimited in time. Attribution (unmodified): "© DLR e.V. 2010-2014 and © Airbus Defence and Space GmbH 2014-2018 provided under COPERNICUS by the European Union and ESA; all rights reserved." Attribution (modified): "produced using Copernicus WorldDEM-30 © DLR e.V. 2010-2014 and © Airbus Defence and Space GmbH 2014-2018 provided under COPERNICUS by the European Union and ESA; all rights reserved." Redistributors must add: "The organisations in charge of the Copernicus programme by law or by delegation do not incur any liability for any use of the Copernicus WorldDEM-30". Provided as is; WorldDEM-10 is excluded [Copernicus DEM license text, R10 §A9] |

The DEM is resampled onto the world grid with bilinear interpolation (the rule is recorded in `Terrain.interpolation`), lane z is taken from the DEM where the source network has no z, and the exporter writes the attribution strings (08 §9).

### 1.5 Provenance fields

`WorldProvenance` (03-interfaces §2) records: source kind and identifier (bbox, file hash, generator id and seed), import date, tool versions (netconvert, osm2streets-style importer version), projection and origin, every transformation with its parameters (simplification tolerance, junction join distance, signal guessing options, height defaulting rule and fit id, DEM resampling), license per layer (road, buildings, terrain), required attribution strings, and the content hash (I-W2). The manifest embeds it; the UI "why" service shows it for any clicked geometry.

## 2. Mobility and actors

Tiers (02-architecture §7.1): `abstract` = kinematic on the lane graph at desired speed, no interaction (ignores queues, signals, lane changes); `medium` = native IDM + MOBIL + gap acceptance + signals + weather (ignores tire dynamics and driver heterogeneity beyond parameter draws); `high` = SUMO co-simulation (Krauss or IDM or Wiedemann, LC2013, junction model, pedestrians). All tiers publish `Kinematics` in the same frame (I-M4) with the constant-velocity extrapolation rule between steps (02 §5.2). Mobility step default 100 ms, range 10-100 ms (ADR 0004); the legacy reference ran at `dt` = 1.0 s [`run.py` L274].

**`mobility/kinematic/lane-follow`** (`Mobility`, abstract). Each actor follows its route at `min(desired_speed, lane speed limit)`, stops for nothing, despawns at trip end. Parameters: desired speed per class (§2.7). Ignores relative to medium: leader interaction, signals, gap acceptance, lane changes, weather. Validation: unit-tested (trip length and time).

### 2.1 Car-following

**`mobility/car-following/idm`** (`CarFollowing`, medium). Purpose: longitudinal acceleration from own speed, gap and speed difference to the leader.

Equations [Treiber 2000 §II; Kesting 2010 §2]:

```
a = a_max · [ 1 − (v/v0)^δ − (s*(v,Δv)/s)^2 ]
s*(v,Δv) = s0 + s1·sqrt(v/v0) + T·v + v·Δv / (2·sqrt(a_max·b))      (s1 = 0 in the base model)
```

with `s` the net gap (leader rear to ego front), `Δv = v − v_lead`. Free-road limit `a = a_max(1 − (v/v0)^δ)`. Legacy port [`run.py` L2274-2283, code (legacy)]: gap floored at 0.5 m after subtracting leader length, `v0` floored at 0.1 m/s, the `s*` term clamped at `s0` (`max(0, ...)`), and the result clamped to `[−6.0, a_max]` m/s². The Kesting 2010 enhancement (constant-acceleration heuristic, "ACC model") adds a coolness factor `c` and is offered as `idm.enhanced = true`.

Parameter sets (all units SI; km/h converted where the source used km/h):

| Parameter | Unit | Treiber 2000 (freeway) | Kesting 2010 car | Kesting 2010 truck | Kesting 2007 (MOBIL study) | SUMO IDM | Legacy default |
|---|---|---|---|---|---|---|---|
| `v0` desired speed | m/s | 33.3 (120 km/h) | 33.3 (120 km/h) | 23.6 (85 km/h) | 33.3 car, 22.2 truck | per vType `maxSpeed` (§2.7) | uniform 8-18 (`trip_speed_min/max`) × class multiplier |
| `T` time headway | s | 1.6 | 1.5 | 2.0 | 1.2 | `tau` 1.0 | 1.3 (`idm_time_headway`) |
| `a_max` | m/s² | 0.73 | 1.4 | 0.7 | 1.5 | per vType `accel` | per class: car 1.8, motorcycle 2.5, truck 0.8, bus 0.9 (`VEHICLE_TYPES`); `cfg.idm_accel` 1.5 is a dead knob |
| `b` comfortable deceleration | m/s² | 1.67 | 2.0 | 2.0 | 2.0 | per vType `decel` | per class: 2.5, 3.0, 1.5, 1.6; `cfg.idm_decel` 2.0 dead knob |
| `s0` jam distance | m | about 2 (implied) | 2.0 | 4.0 | 2.0 | `minGap` 2.5 | 2.5 (`idm_min_gap`) |
| `δ` exponent | 1 | 4 | 4 | 4 | 4 | `delta` 4 | 4 (hard-coded) |
| `c` coolness (enhanced) | 1 | n/a | 0.99 | 0.99 (range 0.95-1.00) | n/a | EIDM `coolness` 0.99 | n/a |
| vehicle length | m | 5 | n/a | n/a | 4 car, 12 truck | per vType | 5.0 (`veh_length_m`), class lengths §2.7 |
| lookahead | m | n/a | n/a | n/a | n/a | n/a | 70 (`idm_lookahead_m`); same-lane leader within lateral 3.0 m and heading difference ≤ 45° [`run.py` L2455-2462] |
| hard deceleration floor | m/s² | n/a | n/a | n/a | n/a | `emergencyDecel` per vType | −6.0 [`run.py` L2283] |
| internal stepping | s | n/a | n/a | n/a | n/a | `stepping` 0.25 | `dt` |

Sources: Treiber 2000 [R10 §B1] (also ρ_jam 140 veh/km, Q_cv 1050 veh/h, jam speed about −15 km/h used in §2.9); Kesting 2010 [R10 §B2]; Kesting 2007 [R10 §B3]; SUMO vType docs [R10 §B4, cache `vehdef.md` L831-835]; legacy [`run.py` L142-146, L429-433, L2274-2283]. The default set for the native medium tier is Kesting 2010 (car and truck) with the legacy class multipliers as an alternative preset `legacy`; the −6 m/s² floor is kept as the parameter `a_min` with source `code (legacy)` and a `TODO: calibrate` plan (compare against SUMO `emergencyDecel` 9 m/s² passenger and 7 m/s² truck [R10 §B4] and against the FHWA yellow-interval deceleration assumption of about 10 ft/s² ≈ 3.05 m/s² [R10 §B10, secondary]).

Assumptions: single leader, no lateral coupling, instantaneous reaction. Limitations: known over-reaction on cut-ins without the enhancement; no reaction time. Ignores (medium relative to high): driver imperfection (`sigma`), action step length, sub-second reaction models. Validation: literature-checked against the fundamental-diagram targets in §2.9.

**`mobility/car-following/krauss`** (`CarFollowing`). SUMO's default model. Description [SUMO vehicle docs, cache `vehdef.md` L903-921]: "let vehicles drive as fast as possibly while maintaining perfect safety (always being able to avoid a collision if the leader starts braking within leader and follower maximum acceleration bounds)", with two documented differences from Krauß 1998: different deceleration capabilities are handled without violating safety, and the safe-velocity formula is discretized for the Ballistic position update. The exact safe-velocity expression is to be transcribed from `MSCFModel_Krauss.cpp` (EPL-2.0, read only, never vendored) at implementation time and quoted in the card; it is not in the research sheets. Parameters: `accel`, `decel`, `emergencyDecel`, `minGap`, `maxSpeed`, `speedDev` per vClass (§2.7); `sigma` driver imperfection 0.5 in [0, 1]; `sigmaStep` = step length; `tau` 1.0 s (net gap, leader back to follower front) [cache `vehdef.md` L829-831]. Tiers: `high` when SUMO runs it; a native port is `medium` and must match SUMO on a golden trajectory (unit test). Ignores: none beyond the SUMO model itself.

**`mobility/car-following/wiedemann-99`** (`CarFollowing`, medium). Psychophysical model with thresholds CC0-CC9 [PTV Vissim help, R10 §B7]:

| Parameter | Unit | Meaning | Default | Status |
|---|---|---|---|---|
| CC0 | m | standstill distance | 1.5 | secondary (WisDOT calibration manual, not opened) UNVERIFIED |
| CC1 | s | headway time | 0.9 | secondary UNVERIFIED |
| CC2 | m | following variation | 4.0 | VERIFIED [PTV Vissim 2023 help] |
| CC3 | s | time before entering braking reaction | `TODO: calibrate` | not quantified on the fetched page |
| CC4, CC5 | m/s | negative and positive speed-difference thresholds | `TODO: calibrate` | not quantified |
| CC6 | 1/(m·s) | distance influence on speed oscillation | `TODO: calibrate` | not quantified |
| CC7 | m/s² | oscillatory acceleration | `TODO: calibrate` | not quantified |
| CC8 | m/s² | acceleration from standstill | `TODO: calibrate` | not quantified |
| CC9 | m/s² | acceleration at 80 km/h | `TODO: calibrate` | not quantified |

Calibration plan for all `TODO` rows: read the PTV Vissim help page family (`FahrverhaltensparameterFolgeverh_Wied99.htm`) or a published Vissim calibration report, record the defaults and the clause. Wiedemann 74 has no consulted source and is not offered [R10 §B7]. The model runs only in the native medium tier when a study needs Vissim-comparable behavior; SUMO does not ship it.

**`mobility/car-following/gipps`** (`CarFollowing`, medium). Structure [Gipps 1981 via Wikipedia, R10 §B8]: free term `v_n(t+τ) ≤ v_n(t) + 2.5·a_n·τ·(1 − v_n(t)/V_n)·(0.025 + v_n(t)/V_n)^½`; congested term = braking-safety bound in `b_n`, `τ`, gap and the estimated leader deceleration `b̂`. Parameters `a_n` (max desired acceleration), `b_n < 0` (most severe braking), `V_n` (desired speed), `s_n` (effective size), `τ` (apparent reaction time): all `TODO: calibrate` (plan: obtain Gipps 1981 Transportation Research B 15(2) and record its Table values; secondary figures seen in search were not confirmed and are not used [R10 §B8]).

### 2.2 Lane change

**`mobility/lane-change/mobil`** (`LaneChange`, medium). Purpose: discretionary and mandatory lane changes from car-following accelerations [Kesting 2007, R10 §B3].

```
safety:     ã_n ≥ −b_safe                                   (new follower after the change)
incentive:  (ã_c − a_c) + p·[(ã_n − a_n) + (ã_o − a_o)] > Δa_th  (+ Δa_bias for asymmetric rules)
```

| Parameter | Unit | Kesting 2007 | Legacy default | Notes |
|---|---|---|---|---|
| `p` politeness | 1 | swept 0 to 1 (0 egoistic, 1 "ideal MOBIL", negative malicious) | 0.2 (`lane_change_politeness`) | default set to 0.2 (`code (legacy)`), `TODO: calibrate` plan: match the lane-change rate band 450-1400 changes/h/km at 10-15 veh/km/lane [Kesting 2007] |
| `Δa_th` threshold | m/s² | 0.1 | 0.2 (`lane_change_threshold`) | |
| `b_safe` | m/s² | 4 (physical max about 9 on dry road) | 4.0 (`_LC_BSAFE`) | |
| `Δa_bias` right-lane bias | m/s² | 0.3 (must exceed `Δa_th`) | none (symmetric) | asymmetric European rule optional |
| minimum speed for discretionary change | m/s | n/a | 3.0 (`_LC_MIN_SPEED`) | code (legacy) [`run.py` L2257] |
| lateral transition duration | s | n/a | 2.5 (`lane_change_time_s`), stretched so peak lateral velocity `1.5·Δ/T` keeps heading deviation ≤ 12° (`_LC_MAXDEV_TAN`) | smoothstep `p²(3 − 2p)` profile [`run.py` L2361-2401] |
| cooldown | s | n/a | `max(2·T_lc, 2·dur)` | anti-oscillation |
| reconsideration probability per step | 1 | n/a | `min(1, 0.25·dt)` | staggers changes [`run.py` L2255] |
| lane classification window | m | n/a | half lane width `0.5 · lane_width_m`, neighbors within `(0.5, 1.5]` lane widths laterally, same direction (heading difference ≤ 45°) | [`run.py` L2306-2318] |

Underlying IDM set in the MOBIL study: T 1.2 s, a 1.5 m/s², b 2 m/s², s0 2 m, cars 120 km/h and 4 m, trucks 80 km/h and 12 m, 20 % trucks, ±20 % speed heterogeneity [Kesting 2007, R10 §B3]. Assumptions: the change is instantaneous for the incentive test, then integrated laterally over the transition. Limitations: no cooperative or strategic (route-driven) changes in the legacy form; the native tier adds a strategic term (lane required by the route within `lcStrategicLookahead`). Ignores (medium relative to high): SUMO's cooperative helping, keep-right, speed-gain lookahead, sublane model.

**`mobility/lane-change/lc2013`** (`LaneChange`, high, via SUMO). Defaults [SUMO vehicle docs, R10 §B5]:

| Parameter | Default | Range | Meaning |
|---|---|---|---|
| `lcStrategic` | 1.0 | [0, ∞), −1 disables | eagerness for route-following changes |
| `lcCooperative` | 1.0 | [0, 1], −1 disables | willingness to yield |
| `lcSpeedGain` | 1.0 | [0, ∞) | eagerness to change for speed |
| `lcKeepRight` | 1.0 | [0, ∞) | keep-right rule |
| `lcContRight` | 1.0 | [0, 1] | choose rightmost lane on lane increase |
| `lcOvertakeRight` | 0 | [0, 1] | violate no-overtake-on-right |
| `lcOpposite` | 1.0 | [0, ∞) | opposite-direction overtaking |
| `lcStrategicLookahead` | 3000 m | [0, ∞) | best-lane computation lookahead |
| `lcSpeedGainRight` | 0.1 | [0, ∞) | right vs left speed-gain asymmetry |
| `lcSpeedGainLookahead` | 0 (LC2013), 5 (SL2015) | [0, ∞) | anticipation time, s |
| `lcSpeedGainRemainTime` | 20 s | [0, ∞) | minimum time on the new lane |
| `lcSpeedGainUrgency` | 50 | [0, ∞) | urgent speed-gain threshold |
| `lcAssertive` | 1 | > 0 | required gap divided by this |
| `lcSigma` | 0.0 | | lateral imperfection |
| `lcCooperativeHelpTime` | 60 s | | yielding time threshold |

### 2.3 Intersection control

**`mobility/intersection/gap-acceptance-hcm`** (`IntersectionControl`, medium). A minor-stream vehicle enters when the gap to the next conflicting major-stream vehicle exceeds the critical gap `t_c`, and successive vehicles follow at the follow-up time `t_f`. Base values (HCM, reproduced by the PTV VISUM help; flagged secondary because the HCM and FHWA PDFs did not parse [R10 §B9]):

| Movement | `t_c` (s), major flow < 4 lanes | `t_c` (s), ≥ 4 lanes | `t_f` (s) |
|---|---|---|---|
| Major-street left turn | 4.1 | 4.1 | 2.2 |
| Minor-street right turn | 6.2 | 6.9 | 3.3 |
| Minor-street through | 6.5 | 6.5 | 4.0 |
| Minor-street left turn | 7.1 | 7.5 | 3.5 |

SUMO junction parameters for the high tier [R10 §B6]: `jmCrossingGap` 10 m, `jmIgnoreKeepClearTime` −1, `jmIgnoreFoeProb` 0, `jmIgnoreFoeSpeed` 0, `impatience` 0.0 growing with `--time-to-impatience` 180 s, pedestrian `timeToMaxImpatience` 120 s. Legacy rule (kept as preset `legacy-closest-first`, `code (legacy)` [`run.py` L2415-2440]): at an unsignalized node the closest claimant within `idm_lookahead_m` has priority, ties by lower vehicle id; a conflicting claimant (heading difference in (45°, 135°)) with higher priority makes the vehicle treat the node as a virtual stopped leader 2 m before the line; no random draw. Ignores (medium relative to high): impatience growth, probabilistic right-of-way violation, pedestrian crossing gaps.

**`mobility/intersection/signal-fixed-time`** (`IntersectionControl`, medium). Fixed-time plans generated when the source has none:

| Parameter | Unit | Default | Source |
|---|---|---|---|
| cycle length | s | 90 | SUMO netconvert `--tls.cycle.time` [R10 §B10]; NACTO recommends 60-90 s for urban streets and FHWA rule of thumb 60 s (2 critical phases) or 75 s (3) [secondary, not opened, R10 §B10] |
| green per phase | s | 31 | netconvert `--tls.green.time` |
| red with no conflicting flow | s | 5 | netconvert `--tls.red.time` |
| yellow | s | auto from kinematics with `--tls.yellow.min-decel` 3 m/s² (netconvert default `-1`); guidance 3 to 6 s, longer on faster approaches, Table 5-7 spanning 3.0 to 5.4 s for 25-60 mph [FHWA Signal Timing Manual 2008 Ch. 5, R10 §B10]; ITE formula `y = t + v/(2a + 2Gg)` with `t` ≈ 1 s, `a` ≈ 10 ft/s² (secondary, not re-verified) | |
| all-red | s | 0 (netconvert `--tls.allred.time`); MUTCD guidance should not exceed 6 s; practitioner values 0.5-2.0 s [FHWA Ch. 5, R10 §B10] | |
| pedestrian crossing minimum green, clearance | s | 4, 5 | netconvert `--tls.crossing-min.time`, `--tls.crossing-clearance.time` |
| left-turn phase | s | 6 | netconvert `--tls.left-green.time` |
| variable phase min, max | s | 5, 50 | netconvert `--tls.min-dur`, `--tls.max-dur` |

Assumptions: pre-timed, no actuation. Ignores (medium relative to high): actuated and coordinated control, SUMO's `request` conflict evaluation inside the junction.

**`mobility/intersection/roundabout-fhwa`** (`IntersectionControl`, medium). Yield-at-entry with the gap-acceptance model above, plus capacity checks from [FHWA Roundabouts Guide Ch. 4, R10 §B17]: single-lane approach maximum circulating flow 1,800 veh/h before a double lane is needed; single-lane exit ceiling 1,200 veh/h (practical 1,200-1,300; theoretical about 1,400); design degree of saturation ceiling 0.85; passenger-car equivalents car 1.0, single-unit truck or bus 1.5, truck with trailer 2.0, bicycle or motorcycle 0.5; short-lane capacity multiplier by vehicle spaces `n_f`: 0 → 0.500, 1 → 0.707, 2 → 0.794, 4 → 0.871, 6 → 0.906, 8 → 0.926, 10 → 0.939; queue estimate `L = v·d/3600`. These are validation targets for the roundabout junction, not behavioral parameters.

**`mobility/intersection/two-coloring-legacy`** (`IntersectionControl`, abstract). Each junction has a stable 2-coloring phase (adjacent junctions alternate on any topology; on a grid equal to `(i+j) mod 2`); each axis is green for half of `light_cycle_s` = 24 s, offset by phase; vehicles halt 2 m before the stop line [`run.py` L2267-2272, L2466-2470, code (legacy)]. Turn slowdown: cap `turn_speed_mps` 6.0 at bends ≥ `turn_min_angle_deg` 40° within lookahead [`run.py` L2479-2483]. Ignores everything above (yellow, all-red, actuation, pedestrians); kept for parity tests.

### 2.4 Routing and demand

**`mobility/routing/dijkstra`** (`Router`). Shortest path on the lane graph with edge cost = length / speed limit; closures raise cost to infinity [`roads.py` `CustomNetwork`, 01-inventory §3.4]. **`mobility/routing/dynamic-reroute`**: re-plan at the next junction when a closure event or a travel-time update (from `EdgeCost`) changes the best route; policy `ReroutePolicy { on_closure, periodic(s) }`. Ignores (all tiers): route choice heterogeneity (only one shortest path); SUMO's `duarouter` and rerouters serve the high tier.

**`mobility/demand/poisson-thinned`** (`Demand`). Candidate arrivals at rate `arrival_rate · cand_boost` are thinned by the time-of-day multiplier and event multipliers [`run.py` L1705-1730, code (legacy)]:

```
rush:   m(f) = 0.2 + 0.8 · min(1, exp(−((f − 0.25)/0.09)²) + exp(−((f − 0.75)/0.09)²))
night:  m(f) = 0.15 + 0.25 · f
uniform: m(f) = 1
```

with `f = t / duration` in [0, 1]. Parameters: `arrival_rate` 2.0 veh/s, `max_total_vehicles` 0 (unlimited), trip speed uniform in [`trip_speed_min` 8, `trip_speed_max` 18] m/s, all `code (legacy)`. Role coins (attacker, faulty, colluder) are drawn in the threat layer, not here. When `t0` is a wall-clock date (constitution), `f` is replaced by clock hour / 24 so the rush peaks fall at 06:00 and 18:00; this mapping is a design choice marked `TODO: calibrate` (plan: replace the two Gaussians by an hourly profile from a public count dataset for the Phase 2 city).

**`mobility/demand/od-gravity`** (`Demand`). Destination law `uniform` or `gravity` with hop-decay scale `od_gravity_scale` 2.0; `boundary_origins` places origins on the perimeter; in `rush` the destination is the network center with probability `m(f) − 0.2` [`run.py` L1741-1743, code (legacy)].

**`mobility/demand/tr36885-drop`** (`Demand`). 3GPP evaluation drops for radio validation runs: spatial Poisson, same-lane inter-vehicle distance mean = 2.5 s × speed; urban speeds 15 or 60 km/h, freeway 70 or 140 km/h; pedestrians 3 km/h [TR 36.885 Table A.1.2-1, R2c]. TR 37.885 variant: bumper-to-bumper gap `max{2 m, Exp(mean = 2 s × speed)}`; highway Option A 140 km/h (70 optional), Option B per lane 80/100/140/40/30/20 km/h, Option C clustered Type-3 platoons of 6 with 2 m gaps; urban Option A 60 km/h, Option B east-west lanes 60/50/25/15 km/h [TR 37.885 §6.1.2, R2c]. Densities for C-V2X validation: 50, 100, 200 veh/km (Todisco), 60 and 120 veh/km (Molina-Masegosa) [R2d].

### 2.5 VRU mobility

**`vru/pedestrian/social-force`** (`VruMobility`, medium). Helbing and Molnár force model [Helbing 1995, R10 §B11]: `dw_α/dt = F_α + fluctuations`, `F_α` = desired-direction relaxation `(v0·e − v)/τ` + pairwise repulsion + border repulsion + optional attraction.

| Parameter | Unit | Default | Source |
|---|---|---|---|
| desired speed mean, std | m/s | 1.34, 0.26 (Gaussian) | Helbing 1995 citing Henderson 1971/1974 |
| maximum speed | m/s | 1.3 × v0 | Helbing 1995 |
| relaxation time `τ` | s | 0.5 | Helbing 1995 |
| pedestrian-pedestrian potential `V0`, decay `σ` | m²/s², m | 2.1, 0.3 | Helbing 1995 |
| pedestrian-border potential `U0`, decay `R` | m²/s², m | 10, 0.2 | Helbing 1995 |
| step-width `Δt` (elliptical potential) | s | 2 | Helbing 1995 |
| field of view `2φ` | degrees | 200 | Helbing 1995 |
| behind-view weight `c` | 1 | 0.5 | Helbing 1995 |

Pedestrians walk on sidewalk lanes and crossings from the world; vehicles are borders with `jmCrossingGap` 10 m as the vehicle-side blocking threshold [R10 §B13]. Ignores (medium relative to high): group behavior, jam states, SUMO's stripe discretization.

**`vru/pedestrian/striping-sumo`** (`VruMobility`, high via SUMO) [SUMO pedestrian docs, R10 §B13]: `--pedestrian.model striping` (alternatives `nonInteracting`, `jupedsim`); stripe width 0.65 m; dawdling 0.2; jam time 300 s (crossing 10 s, narrow 1 s), jammed pedestrians move at a quarter of maximum speed ignoring obstacles; oncoming reservation 1/3 of width at junctions and crossings, off on normal lanes.

**`vru/cyclist/lane-follow`** (`VruMobility`, all): cyclists use bike lanes or the rightmost lane with the IDM and the `bicycle` class of §2.7 (desired 20 km/h, physical 50 km/h) [R10 §B12]; e-scooter desired 20 km/h, max 25 km/h; moped 45 km/h. Legacy VRU (straight-line wander at `vru_speed_mps` 1.8 m/s, no bounds) is reference only (01-inventory §3.3).

### 2.6 Weather effects on driving

**`weather/driving/fhwa-table`** (`WeatherModel`, `driving_effects`). `DrivingEffects { desired_speed_factor, headway_factor, decel_cap, visibility_m }` from [FHWA Road Weather Management, R10 §B14]:

| Condition | Speed reduction | Capacity reduction | Applied as |
|---|---|---|---|
| Freeway, light rain or snow | 3-13 % | 4-11 % | `desired_speed_factor` = 1 − midpoint of the speed band; `headway_factor` = 1 / (1 − midpoint of the capacity band) |
| Freeway, heavy rain | 3-16 % | 10-30 % | same |
| Freeway, heavy snow | 5-40 % | 12-27 % | same |
| Freeway, fog | 10-12 % | 12 % | same; `visibility_m` `TODO: calibrate` (plan: use the FHWA fog visibility classes when the primary table is fetched) |
| Arterial, wet pavement | 10-25 % | saturation flow −2 to −21 % | speed factor on arterial lanes |
| Arterial, snowy or slushy | 30-40 % | volume −15 to −30 % | speed factor |

Using the band midpoint is a design choice recorded in the card; the bands themselves are the cited values. `decel_cap`: `TODO: calibrate` (plan: friction-coefficient tables for wet and icy pavement from an AASHTO or FHWA source).

**`weather/driving/legacy-multipliers`** (`WeatherModel`, abstract preset, `code (legacy)` [`run.py` L137-139]): desired-speed multiplier clear 1.0, rain 0.85, fog 0.75, snow 0.6; GNSS error multiplier 1.0, 1.5, 2.0, 2.5 (used by §2.10); abstract radio extra loss 0.0, 0.03, 0.02, 0.06 (used by §4.9). Ignores: capacity and headway effects, visibility.

### 2.7 Vehicle classes and dimensions

**`mobility/classes/sumo-vtypes`** (data) [SUMO Vehicle Type Parameter Defaults, R10 §B4]:

| vClass | length m | width m | height m | mass kg | minGap m | accel m/s² | decel m/s² | emergencyDecel m/s² | maxSpeed km/h | speedDev |
|---|---|---|---|---|---|---|---|---|---|---|
| passenger | 5 | 1.8 | 1.5 | 1500 | 2.5 | 2.6 | 4.5 | 9 | 200 | 0.1 |
| emergency | 6.5 | 2.16 | 2.86 | 5000 | 2.5 | 2.6 | 4.5 | 9 | 200 | 0 |
| delivery | 6.5 | 2.16 | 2.86 | 5000 | 2.5 | 2.6 | 4.5 | 9 | 200 | 0.05 |
| truck | 7.1 | 2.4 | 2.4 | 4500 | 2.5 | 1.3 | 4 | 7 | 130 | 0.05 |
| trailer | 16.5 | 2.55 | 4 | 13000 | 2.5 | 1.0 | 4 | 7 | 130 | 0.05 |
| bus | 12 | 2.5 | 3.4 | 12000 | 2.5 | 1.2 | 4 | 7 | 85 | 0.1 |
| coach | 14 | 2.6 | 4.0 | 25000 | 2.5 | 2.0 | 4 | 7 | 100 | 0.05 |
| motorcycle | 2.2 | 0.9 | 1.5 | 200 | 2.5 | 6 | 10 | 10 | 200 | 0.1 |
| moped | 2.1 | 0.8 | 1.7 | 80 | 2.5 | 1.1 | 7 | 10 | 45 | 0.1 |
| bicycle | 1.6 | 0.65 | 1.7 | 10 | 0.5 | 1.2 | 3 | 7 | 50 (desired 20) | 0.1 |
| pedestrian | 0.215 | 0.478 | 1.719 | 70 | 0.25 | 1.5 | 2 | 5 | 37.58 (desired 5) | 0.1 |
| scooter | 1.2 | 0.5 | 1.7 | 10 | 0.5 | 1.2 | 3 | 7 | 25 (desired 20) | 0.1 |

Non-road classes (tram 22 × 2.4 × 3.2 m, rail variants, ship) exist in SUMO [R10 §B16] and are out of scope. **`mobility/classes/tr37885-types`**: Type 1 car 5 × 2.0 × 1.6 m antenna 0.75 m; Type 2 car 5 × 2.0 × 1.6 m antenna 1.6 m; Type 3 truck or bus 13 × 2.6 × 3 m antenna 3 m [TR 37.885 §6.1.3, R2c]; TR 36.885 antenna height 1.5 m [R2c]. **`mobility/classes/legacy-fleet`** (`code (legacy)` [`run.py` L142-146]): car speed multiplier 1.00, length 4.5 m, a 1.8, b 2.5, weight 0.75; motorcycle 1.10, 2.2 m, 2.5, 3.0, 0.08; truck 0.80, 12.0 m, 0.8, 1.5, 0.10; bus 0.85, 12.0 m, 0.9, 1.6, 0.07. The `Dims` in `Kinematics` come from these tables; the reference point is the rear-axle center (03 §1).

### 2.8 SUMO co-simulation tier

**`mobility/sumo/traci-cosim`** (`Mobility`, high; crate `v2xw-mobility-sumo`, ADR 0005). What it replaces: the whole native medium stack (§2.1-2.5) with SUMO's Krauss or IDM or EIDM car-following, LC2013 or SL2015 lane change, the junction model with `request` conflict matrices and impatience, pedestrians (striping or jupedsim), and its routing (`duarouter`, rerouters). What stays native: everything else (radio, nodes, security, threats). The engine drives SUMO step by step with a fixed step equal to the engine mobility step (I-M3), reads positions, speeds, accelerations, headings, lane ids and signal states, and writes commands (route change, speed cap, stop) through `MobilityCommand`.

Determinism caveats stated only from what R10 supports: SUMO is out of process (EPL-2.0, never vendored), so its version and seed must be recorded in the manifest (I-M3); the engine hashes SUMO's replies into the run digest so a nondeterministic remote is detected (03 §16). The TraCI versus libsumo transport choice, SUMO's documented determinism guarantees and their known breakers, and the pinned-seed rules the adapter enforces are in ADR 0005 (from R8 §A.2); SUMO's per-step wall-clock overhead is unpublished and is measured in Phase 3.

### 2.9 Fundamental-diagram validation targets

Native medium-tier runs on the `tr36885-freeway` and `todisco` highway worlds must reproduce [R10 §B15]:

| Quantity | Target | Source |
|---|---|---|
| Capacity drop at a freeway bottleneck | about 20 % (order of magnitude); literature range 5-20 %; IDM+ACC simulation 5-15 % | Treiber 2000; Kerner and Rehborn 1996 and Cassidy and Bertini 1999 as cited in Kesting 2010 |
| Jam density | 140 veh/km (IDM calibration) | Treiber 2000 |
| Jam propagation speed | about −15 km/h | Treiber 2000 (also empirical German freeway data in the same paper) |
| Convective-stability flow threshold | about 1,050 veh/h | Treiber 2000 |
| Theoretical maximum flow | `Q_max = (1/T)·(1 − l_eff/(v0·T + l_eff))` | Kesting 2010 Eq. 4.1 |
| Uncongested constant-speed regime | roughly 300-2,200 passenger cars per hour per lane | Hall, FHWA Traffic Flow Theory Ch. 2 |
| Time-mean vs space-mean speed | time-mean 6-12 % greater on a mixed-speed signalized dataset (Wardrop 1952); minimal on uncongested freeways | FHWA Traffic Flow Theory Ch. 2 |
| Roundabout capacities | §2.3 values | FHWA Roundabouts Guide Ch. 4 |

The test computes flow-density and speed-density scatter over 1-minute bins per lane, fits the triangular diagram, and reports capacity, jam density, wave speed and capacity drop against the bands above; a run outside every band marks the parameter set `validation.status = unit-tested` rather than `literature-checked`.

### 2.10 GNSS and clock

**`gnss/error/ou-bias-legacy`** (`GnssModel`). Ported from the legacy `measure` [`run.py` L1874-1897, code (legacy)], with the inventory fixes: the confidence no longer uses the true bias (an oracle quantity), weather is injected through `GnssEnv`, and speed and heading get noise.

```
bias_{k+1} = a·bias_k + q·N(0,1),   a = exp(−dt/τ_b),   q = σ_b · sqrt(1 − a²)      (per axis, OU process)
σ = σ_w · quality · weather_mult · (degrade_factor if in burst else 1)
pos = true + bias + N(0, σ) (+ outlier of magnitude m_o at a uniform angle with probability p_o)
semi_major = semi_minor = 2.448 · sqrt(σ_nom² + σ_b_est²)     (95 % circle; legacy used the true bias, replaced by the model's own bias variance)
```

| Parameter | Unit | Default | Source |
|---|---|---|---|
| `gps_sigma_m` white noise | m | 1.2 | code (legacy) L297 |
| `gps_bias_sigma_m`, `gps_bias_tau_s` | m, s | 1.5, 20 | code (legacy) L298-299 |
| `gps_outlier_rate`, `gps_outlier_mag_m` | 1, m | 0.01, 12 | code (legacy) L300-301 |
| `gps_degrade_rate`, `gps_degrade_factor`, `gps_degrade_dur_s` | 1/step, 1, s | 0.006, 6, 3 | code (legacy) L302-304 |
| `gps_jam_rate`, `gps_jam_dur_s` | 1/step, s | 0.0, 4 | code (legacy) L305-306 |
| per-vehicle quality | 1 | `0.5 + Exp(λ = 1.2)` | code (legacy) L310-311 |
| weather multiplier | 1 | §2.6 legacy values | code (legacy) L137 |
| faulty-sensor bias multiplier | 1 | 5.0 | code (legacy) L313 |

All defaults are `code (legacy)` with one shared `TODO: calibrate` plan: fit σ_w, σ_b, τ_b against the Rayleigh calibration the dataset toolchain already runs (`datagen/calibration.py`, 01-inventory §3.5) on a public GNSS error trace. **`clock/drift/none`** (`ClockModel`): node time equals GNSS time when the fix is valid; drift-free. The sourced replacements, `gnss/error/gauss-markov` (measured quantiles) and `clock/drift/tcxo-ocxo` (oscillator drift), are in §3.8 and are the defaults for the medium and high tiers; the legacy model stays the abstract-tier preset and the parity oracle.

## 3. Propagation, fading, obstacles, weather attenuation, antennas, GNSS

All models here implement `Propagation`, `Fading`, `ObstacleModel`, `GnssModel` or `ClockModel` (03-interfaces §2-4) and return a `LossBreakdown { path_db, shadow_db, obstacle_db, weather_db, antenna_db, total_db }` so the inspector can show every term. Carrier 5.9 GHz (λ ≈ 0.0508 m); the 3GPP evaluation TRs use 6 GHz as a proxy [TR 36.885 Table A.1.1-1 note; TR 37.885 Table 6.1.1-1, R2c].

Tiers (02-architecture §7.1) and what each ignores:

| Tier | Composition | Ignores relative to the next tier |
|---|---|---|
| `abstract` | distance-only preset curve `P_rx(d, load)` calibrated to `high` (§4.9); no geometry | everything below: environment, shadowing realizations, obstacles, fading |
| `medium` | large-scale path loss per environment class (§3.2 or §3.3) + log-normal shadowing with spatial correlation + building and vehicle obstacle shadowing (§3.5) + antenna gain scalars | fast fading (§3.4), terrain diffraction, antenna patterns, weather attenuation |
| `high` | medium + Nakagami-m fast fading per link + terrain knife-edge diffraction + antenna patterns + weather attenuation term (kept for completeness even though it is negligible at 5.9 GHz, §3.6) | frequency-selective fading within the 10 MHz channel, Doppler-dependent channel estimation loss (partly absorbed by the implementation-loss offset in §4.7), polarization mismatch |

### 3.1 Free-space and two-ray ground reflection

**`propagation/free-space`** (`Propagation`, all tiers as the LOS floor). Friis: `L_fs[dB] = 20·log10(d_km) + 20·log10(f_MHz) + 32.44`, equivalently `P_r = P_t·G_t·G_r·(λ/4πd)²` [Sommer 2011 Eq. 1-2, R3 §A.1]. Parameters: none beyond frequency. Conformance: monotone non-decreasing loss with distance (03 §17).

**`propagation/two-ray-ground`** (`Propagation`, medium, high). Below the crossover distance use Friis; above it use the d⁻⁴ asymptote `P_r/P_t = G_t·G_r·h_t²·h_r²/d⁴`, i.e. `L[dB] = 10·log10(d⁴·L_sys/(h_t²·h_r²))` [ns-3 `TwoRayGroundPropagationLossModel`, R3 §A.1]. Crossover `d_c = 4π·h_t·h_r/λ` [ns-3 line 406, R3 §A.1]; Fresnel-corrected breakpoint variant `d_b = (4·h_t·h_r − λ²/4)/λ`, which gives 161 m for `h = 1.47 m`, λ = 0.0536 m (5.6 GHz), while Abbas used 104 m to fit the data [Abbas 2015, R3 §A.1]. Karedal found the two-ray model valid in rural settings for `d ≥ 20 m` [Karedal 2011 Eq. 3, R3 §A.4]. The Veins two-ray interference variant with reflection coefficient `Γ = (sinθ − sqrt(ε_r − cos²θ))/(sinθ + sqrt(ε_r − cos²θ))` is offered as `two-ray-interference`; its default ground permittivity ε_r is UNVERIFIED (not in cache) and is `TODO: calibrate` (plan: read `TwoRayInterferenceModel.ned` from the Veins repository and record the shipped ε_r). Antenna heights: vehicle 1.5 m [TR 36.885, R3 §A.1] (Veins default `antennaOffsetZ` 1.895 m [R3 §A.6]); RSU 5 m [TR 36.885, R3 §F.3].

### 3.2 Log-distance path loss with log-normal shadowing

**`propagation/log-distance-shadowing`** (`Propagation`, medium, high). `PL(d)[dB] = PL0 + 10·n·log10(d/d0) + X_σ`, `X_σ ~ N(0, σ²)`, `d0 = 10 m` [Karedal 2011 Eq. 5; Abbas 2015 Eq. 4, R3 §A.2]. Shadowing is spatially correlated per link: `S(n) = exp(−D/D_corr)·S(n−1) + sqrt(1 − exp(−2D/D_corr))·N(n)` with `D` the distance moved since the last update [TR 36.885 Annex A.1.4, R2c and R3 §A.1]; decorrelation distance urban 10 m, freeway 25 m for V2V [TR 36.885 Annex A.1.4 as read in R2c], 50 m for the eNB-UE link [TR 36.885, R3 §A.1]. Draws come from the `Shadow` RNG domain keyed by `LinkKey` (02 §6.2).

Environment presets (dual-slope form `n1` below the breakpoint `d_b`, `n2` above; the Abbas table prints channel-gain exponents, so path-loss exponents are their magnitudes):

| Preset id | State | n1 | n2 | PL0 at d0 = 10 m (dB) | σ (dB) | d_b (m) | Source and status |
|---|---|---|---|---|---|---|---|
| `abbas-los-highway` | LOS | 1.66 | 2.88 | 66.1 | 3.95 | 104 | [Abbas 2015 Table II, R3 §A.5] VERIFIED |
| `abbas-los-urban` | LOS | 1.81 | 2.85 | 63.9 | 4.15 | 104 | [Abbas 2015 Table II] VERIFIED |
| `abbas-olos-highway` | OLOS (vehicle obstructed) | not modeled (too few short-range samples) | 3.18 | 76.1 | 6.12 | 104 | [Abbas 2015 Table II] VERIFIED |
| `abbas-olos-urban` | OLOS | 1.93 | 2.74 | 72.3 | 6.67 | 104 | [Abbas 2015 Table II] VERIFIED |
| `abbas-nlos-intersection` | NLOS (building) | 2.69 (single slope, Mangel model) | | | 4.1 | | [Abbas 2015 Eq. 6, R3 §A.5] VERIFIED |
| `kunisch-highway`, `kunisch-urban` | LOS | 1.85, 1.61 | | | 3.2, 3.4 | | Kunisch and Pamp as quoted by [Karedal 2011, R3 §A.4]; secondary |
| `cheng-highway` | LOS | 1.9 | UNVERIFIED | | UNVERIFIED | 220 | [Cheng 2007 via Karedal 2011, R3 §A.3]; primary unreachable; n2 and σ UNVERIFIED |
| `cheng-suburban-a`, `-b` | LOS | 2.0-2.1; 2.3 (range 2.3-2.75 corroborated) | UNVERIFIED | | UNVERIFIED | 100; 226 | [Cheng 2007 via Karedal 2011 and Boban thesis, R3 §A.3] secondary |
| `karedal-*` | all four environments | n < 2 qualitatively | | UNVERIFIED | UNVERIFIED | | [Karedal 2011 Table I] numeric grid UNVERIFIED (extraction failed on three copies); not shippable until read |

Abbas measured the LOS to OLOS offset at 8.6-10 dB, cross-checked against 9.6 dB and 10-20 dB in other work [Abbas 2015, R3 §A.5]; this is the value the OLOS presets encode implicitly and the vehicle-obstacle model of §3.5 makes explicit. Registry rule: a preset with any UNVERIFIED constant is registered `validation.status = unvalidated` and the scenario validator warns when it is used in a `high` run.

Legacy abstract radio constants (`code (legacy)` [`run.py` L340-355]): `radio_range_m` 500, `pathloss_exponent` 2.7, `shadowing_sigma_db` 4.0, `rx_sensitivity_margin_db` 0, mean received power relative to sensitivity `10·n·log10(rr/d)` so the median range equals `radio_range_m`; they parameterize `phy/abstract/logdistance-legacy` (§4.9), not this model.

### 3.3 3GPP evaluation channel models

**`propagation/tr37885`** (`Propagation` + `ObstacleModel` state machine, medium, high). Link state LOS, NLOSv (same street, blocked by a vehicle) or NLOS (different streets); re-evaluated at every 100 ms location update [TR 37.885 §6.2, R2c].

```
Urban:   P(LOS) = min{1, 1.05·exp(−0.0114·d)};  P(NLOSv) = 1 − P(LOS) for same-street links; NLOS is geometric (different streets)
Highway: d ≤ 475 m: P(LOS) = min{1, 2.1013e−6·d² − 0.002·d + 1.0193};  d > 475 m: P(LOS) = max{0, 0.54 − 0.001·(d − 475)};  no NLOS state
PL_highway,LOS/NLOSv = 32.4  + 20.0·log10(d3D) + 20.0·log10(fc_GHz)     σ_SF = 3 dB
PL_urban,LOS/NLOSv   = 38.77 + 16.7·log10(d3D) + 18.2·log10(fc_GHz)     σ_SF = 3 dB
PL_NLOS              = 36.85 + 30.0·log10(d3D) + 18.9·log10(fc_GHz)     σ_SF = 4 dB
```

[TR 37.885 Tables 6.2-1 and 6.2.1-1, R2c and R3 §D.4]. NLOSv adds the vehicle blockage loss of §3.5. Shadowing: log-normal, σ per state above, decorrelation and update rule as in §3.2. The infrastructure links (B2V, B2R) reuse TR 38.901 UMa (urban) and RMa (highway) with `hE = 0.25 m` [TR 37.885 Table 6.2.1-2, R2c]; TR 38.901 is not cached, so those constants are `TODO: calibrate` (plan: read TR 38.901 Table 7.4.1-1 and record UMa and RMa coefficients). eNB-UE (Uu) path loss from the LTE study: `128.1 + 37.6·log10(R_km)`, σ 8 dB, decorrelation 50 m, SCM NLOS fast fading [TR 36.885 Table A.1.4-2, R2c].

**`propagation/winner-plus-b1`** (`Propagation`, medium, high). TR 36.885 names WINNER+ B1 (Manhattan grid, antenna 1.5 m, path loss at 3 m used below 3 m) with σ 3 dB LOS and 4 dB NLOS [TR 36.885 Table A.1.4-1, R2c]; the B1 constants live in the WINNER II D1.1.2 report, which is not cached, so this model is registered `unvalidated` with all coefficients `TODO: calibrate` (plan: transcribe the B1 LOS and NLOS formulas from WINNER II D1.1.2 Table 4-4). Validation runs that follow Molina-Masegosa, Bazzi or Todisco (§13) need it.

### 3.4 Fast fading

**`fading/nakagami-m`** (`Fading`, high). Power gain drawn from `f(x; m, Ω) = 2m^m/(Γ(m)Ω^m)·x^(2m−1)·exp(−m·x²/Ω)`, `m ≥ 1/2`, `m = 1` Rayleigh, larger `m` more LOS-like [Torrent-Moreno 2009 Eq. 2, R3 §B]. Sampled per link and per frame from the `Fading` RNG domain through the in-crate gamma sampler (03 §1.1).

| Preset | m by distance | Source and status |
|---|---|---|
| `fixed-severe`, `fixed-medium`, `fixed-low` | 1, 3, 5 | [Torrent-Moreno 2009, R3 §B] labels used in D-FPAV and EMDV studies |
| `yin-dsrc-freeway` | 1.0-1.8 for d < 100 m; 0.7-1.0 for d ≥ 100 m (10 m bins) | Yin et al. as quoted in the α-μ DSRC paper [R3 §B]; secondary; not Cheng 2007 |
| `taliwal-ns2` | 3 below 50 m, 1.5 for 50-150 m, 1 beyond | UNVERIFIED (Torrent-Moreno 2009 credits Taliwal's ns-2.28 port but does not reproduce the thresholds; VANET '04 text not retrieved); not shipped until confirmed |
| Cheng 2007 distance-binned m | UNVERIFIED (not recoverable) | |
| Rician K for 5.9 GHz V2V | no numeric value found in any cached source [R3 §B]; `fading/rician` is not offered | |

Default for `high`: `yin-dsrc-freeway` on highways and `fixed-medium` (m = 3) elsewhere, both recorded as `literature-checked` only for the freeway preset. **`fading/none`** serves `medium` (02 §7.1: medium ignores fast fading). Ignores (high relative to reality): frequency selectivity, Doppler spectrum, temporal correlation between successive frames on the same link (each frame draws independently; `TODO: calibrate` plan: add a coherence-time model once a 5.9 GHz Doppler measurement is cited).

### 3.5 Obstacles: buildings, terrain, vehicles, foliage

**`obstacle/building/sommer-2011`** (`ObstacleModel` + `Propagation` term, medium, high). `L_obs[dB] = β·n + γ·d_m` with `n` the number of exterior walls crossed and `d_m` the in-building path length, combined as `P_r = P_t + 10·log10(G_t·G_r·λ²/(16π²·d^α)) − β·n − γ·d_m` [Sommer 2011, R3 §C.1]. The world's R-tree gives `walls_crossed` and `obstructed_len_m` (`LosResult`, 03 §2).

| Building class | β (dB per wall) | γ (dB/m) | Source |
|---|---|---|---|
| default (majority of the dataset) | 9 | 0.4 | [Sommer 2011, R3 §C.1] |
| free-standing warehouse (countryside) | 9.2 | 0.32 | same |
| suburban house | 9.6 | 0.45 | same |
| light-construction house | 2.4 | 0.63 | same |
| urban residential home (per-building fit) | 2.38 | 0.10 | same |
| urban residential garage (per-building fit) | 6.26 | 0.41 | same |

Fitted by Gauss-Newton least squares, tolerance 1e−5 [Sommer 2011, R3 §C.1]. The Veins framework's shipped `SimpleObstacleShadowing` defaults are UNVERIFIED (not in cache) and are not used. Material class from §1.3 selects the row; `unknown` maps to default.

**`obstacle/terrain/knife-edge-p526`** (`ObstacleModel`, high). Single knife edge from the DEM profile: `ν = h·sqrt(2(d1 + d2)/(λ·d1·d2))` (self-consistent units; practical-units form `ν = 0.0316·h·sqrt(2(d1 + d2)/(λ·d1·d2))` with `h`, λ in m and `d1`, `d2` in km) and `J(ν) = 6.9 + 20·log10(sqrt((ν − 0.1)² + 1) + ν − 0.1)` dB for `ν > −0.78` [ITU-R P.526-14 Eq. 26, 31, 33, R3 §C.2]; the exact Fresnel-integral form (Eq. 30) is available as `exact = true`. Multiple edges: ITU-R (modified Epstein-Peterson, more optimistic) or Deygout (more pessimistic) [R3 §C.2]; default Deygout, recorded in the card. The cached edition is P.526-14; the current one is P.526-15.

**`obstacle/vehicle/tr37885-nlosv`** (`ObstacleModel`, medium, high). Extra loss `max{0, lognormal}` by antenna heights versus blocker height [TR 37.885 §6.2.1, R2c and R3 §D.4]: Case 1 (minimum antenna height of Tx and Rx above the blocker) 0 dB; Case 2 (maximum antenna height below the blocker) mean `9 + max(0, 15·log10(d) − 41)` dB, σ 4.5 dB; Case 3 (otherwise) mean `5 + max(0, 15·log10(d) − 41)` dB, σ 4 dB; blocker height drawn from the vehicle-type heights weighted by population share. The engine uses the actual blocker's `Dims` from the actor index instead of a random draw when actors are passed to `los()`.

**`obstacle/vehicle/knife-edge-boban`** (`ObstacleModel`, high). Each obstructing vehicle is a knife edge of height `H` above the Tx-Rx line: `A_sk = 6.9 + 20·log10(sqrt((v − 0.1)² + 1) + v − 0.1)` for `v > −0.7`, else 0, with `v = sqrt(2H/r_f)` and `r_f` the Fresnel radius [Boban thesis Eq. 3.8, R3 §D.3]; valid because λ ≈ 5 cm is much smaller than vehicle dimensions. Measured anchors for validation: a single obstructing vehicle reduces received power by more than 20 dB [Boban 2011 JSAC via R3 §D.1]; a large truck 27 dB at 26 m and a van 12 dB at 20 m [Meireles 2010 via R3 §D.1]; effective range reduced by up to 60 % and PDR by up to 30 %; tall vehicles are on average more than 1.5 m taller than passenger cars [R3 §D.1]. Measured vehicle dimensions (height × width × length, m) for the obstacle library: Lincoln LS 1.453 × 1.859 × 4.925; Pontiac Vibe 1.547 × 1.763 × 4.371; Ford E-250 van 2.085 × 2.029 × 5.504; Kia Cee'd 1.480 × 1.790 × 4.260; Honda Jazz 1.525 × 1.676 × 3.845; Mercedes Sprinter 2.591 × 1.989 × 6.680; Fiat Ducato 2.524 × 2.025 × 5.943; Citroen C4 1.491 × 1.789 × 4.329; Opel Astra 1.510 × 1.814 × 4.419 [Boban thesis Tables 3.7, 4.2, 5.2, R3 §D.2]; tall-vehicle fraction on the A28 14.36 % (32.3 veh/km) and A3 18.18 % (7.3 veh/km) [Boban thesis Table 3.1].

**`obstacle/foliage/boban-mel`** (`ObstacleModel`, high). `MEL = 0.79·f_GHz^0.61` dB/m, 2.3 dB/m at 5.9 GHz for deciduous trees [Boban thesis Eq. 4.1, R3 §D.3]; applied to the path length inside land-use polygons tagged as wooded.

Maximum link ranges used by Boban's calibrated simulator, kept as the abstract-tier cutoff targets per link class: LOS highway 1,000 m, LOS urban 500 m, NLOSv 400 m, NLOSb 300 m [Boban thesis Table 4.5, R3 §D.3].

Ignores: `medium` uses `sommer-2011` and `tr37885-nlosv` only (no diffraction, no foliage); `high` adds `knife-edge-p526`, `knife-edge-boban`, `boban-mel`. Neither models reflections from buildings (multipath enrichment in NLOS) beyond what the NLOS path-loss exponent absorbs.

### 3.6 Weather attenuation

**`weather/attenuation/itu-r`** (`Propagation` term `weather_db`, high). Rain: `γ_R[dB/km] = k·R^α` with the 6 GHz row (closest to 5.9 GHz) `k_H` 0.0007056, `α_H` 1.5900, `k_V` 0.0004878, `α_V` 1.5728 (5.5 GHz row: 0.0003909, 1.6499, 0.0003115, 1.5882) [ITU-R P.838-3 Table 5, R3 §E.1]. Computed in the sheet for R = 50 mm/h over 300 m: horizontal 0.35 dB/km, about 0.11 dB; vertical 0.23 dB/km, about 0.07 dB [R3 §E.1, model output]. Fog: `γ_c = K_l(f, T)·M` with M = 0.05 g/m³ (medium fog, visibility about 300 m) or 0.5 g/m³ (thick fog, about 50 m) [ITU-R P.840-8, R3 §E.2]; `K_l(5.9 GHz)` UNVERIFIED (real part of the double-Debye permittivity not recovered); the recommendation states fog matters at 100 GHz and above. Gases: `γ = 0.1820·f·N''(f)` line by line [ITU-R P.676-12 Eq. 1, R3 §E.3]; the 5.9 GHz value was not computed (UNVERIFIED; the often-quoted 0.01-0.02 dB/km is indicative only).

Conclusion carried from the sheet [R3 §E.4]: rain, fog and gaseous absorption are negligible at 5.9 GHz over V2X distances (a fraction of a decibel in a 50 mm/h downpour over 300 m), against 6-10 dB and more consumed by shadowing, NLOSv blockage or one obstructing truck. The term is kept in `high` so that the breakdown is honest and so that a future mmWave RAT can reuse it; `medium` ignores it. Note that the legacy weather effect on radio (`WEATHER_RADIO_LOSS`, §2.6) is a packet-loss add-on with no physical basis and survives only inside `phy/abstract/*-legacy`.

### 3.7 Antennas, sensitivity, noise

**`propagation/antenna/isotropic-gain`** (all tiers): scalar gains, vehicle UE 3 dBi, UE-type RSU 3 dBi, pedestrian UE 0 dBi [TR 36.885 Table A.1.1-1, R2c]; commercial OBU DSRC antenna 5 dBi omni dipole, two for diversity [Unex OBU-301E, R3 §F.3]; TR 37.885's macro-comparable RSU 23 dBi and vehicle 21 dBi (V2V) or 14 dBi (V2I) are beam-forming assumptions for the above-6 GHz study and are not defaults here [R2 §D.2]. **`propagation/antenna/pattern`** (`high`): azimuth and elevation pattern from a file (`PatternRef`); no cached pattern exists, so shipping patterns are `TODO: calibrate` (plan: digitize one roof-mount shark-fin pattern from an antenna datasheet in R7). Heights: vehicle 1.5 m (TR 36.885) or per TR 37.885 type (0.75, 1.6, 3 m); RSU 5 m (3GPP) with the FCC cap of 8 m at full power and 15 m hard cap with EIRP reduced by `20·log10(H_t/8)` dB above 8 m [FCC 24-123 §90.391(b), R1 §A.1]; CTI 4501 gives only qualitative RSU height guidance [R3 §F.3].

**`phy/receiver/noise-sensitivity`** (`Phy` parameters, all tiers). Thermal noise in 10 MHz `N = −174 + 10·log10(10^7) = −104 dBm` [kTB, R3 §F.2]; noise figure 6 dB (NXP SAF5400 fact sheet, R3 §F.2) giving −98 dBm floor, or 9 dB for 3GPP evaluations [TR 36.885 Annex A.1.1, R2c] giving −95 dBm. Minimum sensitivities per MCS are the EN 302 663 Table 1 values in §4.2; the dynamic (interference-present) figures are 3 dB higher (Table 2, 6 Mbit/s −85 dBm) [R1 §A.5, R3 §F.1]. Commercial cross-check, Cohda MK5 module, 10 MHz, no multipath, one antenna: BPSK 1/2 −98 dBm, BPSK 3/4 −96, QPSK 1/2 −95, QPSK 3/4 −93, 16-QAM 1/2 −90, 16-QAM 3/4 −86, 64-QAM 2/3 −82, 64-QAM 3/4 −80 dBm, about 5-7 dB better than the ETSI minimum and 3-7 dB worse under the synthetic highway NLOS multipath channel [Cohda MK5 module datasheet Table 2, R3 §F.1]. Default: ETSI minimum (standards-exact); preset `cohda-mk5` for hardware-matched studies. LTE-V2X: sensitivity −90.4 dBm, maximum input −22 dBm [TS 36.101 v14.4.0 via R2e].

### 3.8 GNSS error and clock drift

**`gnss/error/gauss-markov`** (`GnssModel`, all tiers; replaces `gnss/error/ou-bias-legacy` of §2.10 as the default). Structure as in §2.10 (first-order Gauss-Markov per axis, `R(Δt) = σ²·exp(−|Δt|/τ)` [generic GNSS/INS formulation, R3 §G.5]) with white noise, outliers, outage bursts and an environment-dependent scale; defaults now come from measurements:

| Parameter | Unit | Default | Source and status |
|---|---|---|---|
| horizontal error 68 / 95 / 99 % (open sky, production automotive single-frequency) | m | 3.07 / 5.30 / 9.38 | [Reid 2019 Table, R3 §G.2] |
| lateral 68 / 95 / 99 % | m | 1.92 / 3.88 / 5.74 | Reid 2019 |
| longitudinal 68 / 95 / 99 % | m | 2.11 / 4.44 / 7.95 | Reid 2019 |
| vertical 68 / 95 / 99 % | m | 4.59 / 9.42 / 12.83 | Reid 2019 |
| RTK reference (OxTS RT3000) horizontal 68 / 95 / 99 % | m | 0.26 / 1.05 / 3.91 | Reid 2019; used for the `rtk` quality class |
| GPS SPS committed accuracy | m | ≤ 8 (95 % horizontal), ≤ 13 (95 % vertical); worst site ≤ 15 / ≤ 33; velocity ≤ 0.2 m/s 95 %; time ≤ 30 ns 95 % | [GPS SPS PS 2020 Table 3.8-3, R3 §G.1]; achieved in 2018 about 3 m horizontal, 5 m vertical 95 % |
| deep urban canyon standalone | m | mean 31.02, std 37.69, max 177.59 (second canyon mean 30.68, std 25.14, max 92.32) | [Wen and Hsu, R3 §G.3]; with NLOS exclusion mean 9.57, std 7.32, availability 96.01 % |
| urban canyon RTK | m | 2D 1.81, 3D 3.65 mean, fix rate about 14 % | [Wen and Hsu UrbanNav, R3 §G.3] |
| SPS outage duration, 95th percentile | s | < 7 | Reid 2019 |
| RTK fixed or float outage, 95th percentile | s | can exceed 60 | Reid 2019 |
| correlation coefficient (discrete) | 1 | `DecayFactor` 0.999 per sample; horizontal accuracy 1.6 m, vertical 3 m | [MathWorks `gpsSensor` defaults, R3 §G.5]; the SA-era τ ≈ 127 s convention is UNVERIFIED and not used |
| lane determination (< 1.5 m) achieved | 1 | 57 % single-frequency, 98 % RTK; road determination (< 5 m) 98 % | Reid 2019 conclusions |
| requirement anchor | m | 1.5 m at 68 % (NHTSA V2V rule, basis of J2945/1) | [NHTSA 2017 via Reid 2019, R3 §G.4]; the Federal Register sentence was not located in the cached text (UNVERIFIED at the primary level) |

Mapping to the model: σ and τ per axis are fitted so that the stationary 68 / 95 / 99 % quantiles reproduce the Reid rows (a deterministic fit run at registration; the fit and residual are stored in the card); the `GnssEnv` environment class scales σ by the ratio of the urban-canyon mean to the open-sky mean (31.02 / 3.07 for `deep-canyon`, 9.57 / 3.07 for `canyon-mitigated`), with the class chosen from the world's building density along the sky view (`TODO: calibrate`, plan: compare against the Hong Kong dataset once a sky-view metric is defined). Outages: duration drawn so that the 95th percentile is 7 s (SPS) or 60 s (RTK). Jamming and spoofing (attacker side, 07-threats): spoofing effective range about 91-547 m with an SDR [arXiv 2606.20215, R3 §G.6]; the "under $50" jammer claim is UNVERIFIED. Ignores (all tiers): satellite geometry, multipath as a function of true building geometry (only a class), receiver filtering dynamics. Speed and heading errors: `TODO: calibrate` (plan: use the GPS SPS velocity bound 0.2 m/s 95 % as the interim σ_v and fit heading noise from the Reid dataset if released).

**`clock/drift/tcxo-ocxo`** (`ClockModel`, medium, high). Node time = GNSS time while the fix is valid (time transfer ≤ 30 ns at 95 % [GPS SPS PS 2020]); in holdover the clock drifts at the oscillator's fractional frequency error: automotive TCXO ±0.5 to ±5.0 ppm over −40 to +85 °C (grade G3) or +105 °C (G2) [TXC TCXO page, R3 §G.7], i.e. 1 ppm = 1 µs/s; PPS-disciplined OCXO ≤ 0.5 ppb (some ≤ 0.1 ppb) with ≤ 1.5 µs phase error over 24-48 h holdover [Rakon PPS-OCXO page, R3 §G.7]. Defaults: OBU `tcxo` with 2 ppm (mid-range of the cited band, recorded as a design choice), RSU `ocxo` with 0.5 ppb; 1PPS discipline is standard on commercial OBUs [Unex OBU-301E; Autotalks CRATON, R3 §G.7]. The believed time feeds `generationTime` and the plausibility windows of §14 (C2C-CC future tolerance 220 ms, past tolerance 2 s [RS 2037, R4 §D]). `clock/drift/none` (§2.10) remains the abstract-tier choice.

## 4. PHY and MAC: IEEE 802.11p / ITS-G5

Models: `phy/80211p/ofdm-10mhz` (`Phy`, medium and high), `phy/80211p/nist-per` (error model), `mac/80211p/edca-ocb` (`Mac`, high), `mac/80211p/slotted-abstraction` (`Mac`, medium), `phy/abstract/distance-load-table` (`Phy`, abstract) and the legacy abstract models. RAT id `Dsrc80211p`.

### 4.1 Channel plans

United States [R1 §A.1; R2d]. Note on document numbers: R1 cites the cached text of the 2020 First Report and Order (ET Docket 19-138) as FCC 20-51, R2 and R2d cite the same order as FCC 20-164 (adopted 2020-11-18); the fact sheet in the cache is dated 2020-10-28. The two sheets describe the same content; which FCC number is correct is UNVERIFIED here and is carried as `fcc-2020-first-ro` in the card until checked against the FCC record.

| Item | Value | Source |
|---|---|---|
| ITS band after reallocation | 5.895-5.925 GHz (upper 30 MHz); 5.850-5.895 GHz unlicensed U-NII-4 | FCC 2020 First R&O ¶24-30, 137, 169 |
| 10 MHz channels | Ch. 180 = 5.895-5.905; Ch. 182 = 5.905-5.915; Ch. 184 = 5.915-5.925 GHz | FCC 2020 First R&O ¶149 |
| 20 MHz combination | Ch. 180 + 182 = Ch. 181 (5.895-5.915 GHz) | ¶149; 47 CFR §90.377(b) |
| C-V2X bandwidth flexibility | 10, 20 or 30 MHz, any combination of the three segments | FCC 24-123 §90.390(a) |
| C-V2X RSU EIRP | 33 dBm per 10, 20 or 30 MHz (RMS) | FCC 24-123 §90.391(a) |
| C-V2X RSU antenna height | ≤ 8 m at full power; 8-15 m with EIRP reduced by `20·log10(H_t/8)` dB; hard cap 15 m | FCC 24-123 §90.391(b) |
| C-V2X OBU EIRP (no geofencing) | Ch. 180: 23 dBm/10 MHz; Ch. 182 and 184: 33 dBm/10 MHz reduced to 27 dBm within ±5° of horizontal; 20 MHz 5.895-5.915: 23 dBm; 20 MHz 5.905-5.925: 33 to 27 dBm near horizon; 30 MHz: 23 dBm | FCC 24-123 §95.3204(a) |
| C-V2X OBU EIRP (geofenced) | 33 dBm | FCC 24-123 §95.3204(b) |
| OOBE limits | −20 dBm/100 kHz at 1 MHz from the edge, −30 at 10 MHz, −40 at 20 MHz | FCC 24-123 [R2 §F] |
| Legacy DSRC RSU EIRP (transitional) | Ch. 180/181/182: 23 dBm; Ch. 184 public safety 33/40 dBm | FCC 24-123 Appendix A §22 amending §90.377(b) |
| Legacy DSRC OBU standard | IEEE 802.11p-2010 | FCC 2020 §95.3189(a) |
| DSRC sunset | two years after Federal Register publication of FCC 24-123 (about December 2026) | [R2d] |

Europe [R1 §A.2]:

| Channel | Center (MHz) | Number | Band | Source |
|---|---|---|---|---|
| ITS-G5B | 5,855-5,875 GHz (non-safety) | | | EN 302 571 V2.1.1 §4.2.1 |
| ITS-G5A | 5,875-5,905 GHz (road safety, EC Decision 2008/671/EC): exactly CCH, SCH1, SCH2 | | | EN 302 571 §4.2.1; TS 102 724 §5.4.1 |
| ITS-G5D | 5,905-5,925 GHz (future ITS) | | | EN 302 571 §4.2.1 |
| CCH / G5-SCH0 | 5,900 | 180 | G5A | C2C-CC RS 2037 RS_BSP_545 |
| SCH2 | 5,890 | 178 | G5A | same |
| SCH1 | 5,880 | 176 | G5A | same |
| SCH3, SCH4 | 5,870, 5,860 | 174, 172 | G5B | same |
| SCH5, SCH6 | 5,910, 5,920 | 182, 184 | G5D | same |
| Maximum e.i.r.p. | 33 dBm any ITS-G5 station | | | EN 302 571 §4.2.2.2 |
| Maximum PSD | 23 dBm/MHz e.i.r.p. (the "23 dBm for G5A" of secondary literature is this PSD limit, not a total-power cap) | | | EN 302 571 §4.2.3.2 |
| TPC range | ≥ 3 dB below maximum | | | EN 302 571 §4.2.4.2 |

Channel switching: IEEE 1609.4 sync interval 100 ms, CCH and SCH intervals 50 ms each, 4 ms guard, 46 ms usable, all UNVERIFIED at the primary level (secondary literature only) [R1 §F]; European ITS-G5 runs continuous single-channel on the G5A CCH or multi-transceiver (TS 102 724) [R1 §F]; Veins defaults to switching (`useServiceChannel = true`) [R1 §F]. Default here: continuous CCH; `mac/80211p/edca-ocb` exposes `channel_switching = {none, 1609-4}` with the 1609.4 constants tagged UNVERIFIED.

### 4.2 OFDM parameters, MCS and sensitivity

Half-clocked OFDM, 10 MHz channels per IEEE 802.11-2016 clause 17; 52 subcarriers (48 data + 4 pilot); symbol 8 µs; preamble 32 µs; SIGNAL 8 µs (24 bits, BPSK 1/2); mandatory rates 3, 6, 12 Mbit/s [EN 302 663 V1.3.1 §4.2, Annex C.3, Table C.2, R1 §A.3].

| Rate (Mbit/s) | Modulation | Coding rate | Data bits per symbol | Coded bits per symbol | Static sensitivity (dBm) | Dynamic sensitivity (dBm) |
|---|---|---|---|---|---|---|
| 3 | BPSK | 1/2 | 24 | 48 | −91 | |
| 4.5 | BPSK | 3/4 | 36 | 48 | −90 | |
| 6 | QPSK | 1/2 | 48 | 96 | −88 | −85 |
| 9 | QPSK | 3/4 | 72 | 96 | −86 | |
| 12 | 16-QAM | 1/2 | 96 | 192 | −83 | |
| 18 | 16-QAM | 3/4 | 144 | 192 | −79 | |
| 24 | 64-QAM | 2/3 | 192 | 288 | −75 | |
| 27 | 64-QAM | 3/4 | 216 | 288 | −74 | |

[EN 302 663 V1.3.1 Table C.1 (rates), Table 1 and Table 2 (sensitivity), R1 §A.4-A.5]; the common "IEEE Table 17-18" attribution is UNVERIFIED. Air time (`Phy::air_time`): `T = 32 µs + 8 µs + 8 µs · ceil((N_SERVICE + 8·bytes + N_TAIL) / N_DBPS)` with `N_SERVICE = 16` and `N_TAIL = 6` bits as used by the cached NIST PER reproduction (`nist_per.py`, R1 §C.1) and by Veins and ns-3; the IEEE clause (17.3.2) is not in the cache, so the two constants are tagged UNVERIFIED at the clause level with the check "IEEE 802.11-2016 clause 17.3.2 and 17.4.3". Default MCS: 6 Mbit/s QPSK 1/2 (the rate measured by Sjöberg and the common safety-channel default); 3 Mbit/s for the Torrent-Moreno capture reference.

### 4.3 EDCA in OCB mode

`mac/80211p/edca-ocb` (`Mac`, high). OCB (`dot11OCBActivated`): no association or authentication, wildcard BSSID, no power save, no beacons (Timing Advertisement frames instead, GPS sync), no group-addressed ACKs, so the backoff is invoked once during the initial listening and `CW` stays at `CWmin` [EN 302 663 Annex C.4.2 note and C.5, R1 §B.1].

| AC | CWmin | CWmax | AIFSN | AIFS (µs) |
|---|---|---|---|---|
| AC_VO | 3 | 7 | 2 | 58 |
| AC_VI | 7 | 15 | 3 | 71 |
| AC_BE | 15 | 1023 | 6 | 110 |
| AC_BK | 15 | 1023 | 9 | 149 |

`AIFS[AC] = AIFSN[AC] × aSlotTime + aSIFSTime`; `aSlotTime` 13 µs, `aSIFSTime` 32 µs, `aCWmin` 15, `aCWmax` 1023 [EN 302 663 Annex C.4.4, Tables C.4-C.6, citing IEEE 802.11-2016 Table 9-138 for the AC parameters and Table 17-21 for the PHY constants, R1 §B.2]. The correct table is 9-138 (not 9-137). UP to AC mapping: UP 1, 2 → AC_BK; UP 0, 3 → AC_BE; UP 4, 5 → AC_VI; UP 6, 7 → AC_VO [EN 302 663 Table C.3]. Veins reference configuration: `useAcks = false`, `dot11RTSThreshold` 12,000 bit (RTS/CTS effectively off), short retry limit 7, long retry limit 4, ACK length 112 bit [Veins `Mac1609_4`, R1 §B.4]. Retries apply only to individually addressed frames (none for BSM, CAM, DENM).

Algorithm (per node, per AC queue): on enqueue with idle medium for AIFS, transmit immediately; otherwise draw backoff uniformly in `[0, CWmin]` slots, count down while idle, freeze while busy (CCA of §4.5), transmit at zero; internal collisions resolved to the higher AC; no retransmission and no CW doubling for group-addressed frames. Timing uses `SimTime` nanoseconds so 13 µs slots and 8 µs symbols are exact (02 §5.1); propagation delay 1 µs per 300 m is applied to arrival start.

### 4.4 Timing constants

| Constant | Value | Source |
|---|---|---|
| aSlotTime, aSIFSTime | 13 µs, 32 µs | EN 302 663 Annex C.4.4 |
| preamble, SIGNAL, symbol | 32 µs, 8 µs, 8 µs | EN 302 663 Annex C.3 |
| CBR window `T_CBR` | 100 ms | EN 302 571 §4.2.10.1; TS 102 687 Table 3 |
| DCC evaluation period | 200 ms (at least every 200 ms) | TS 102 687 §5.2, §5.4 |
| 1609.4 sync, CCH, SCH, guard | 100, 50, 50, 4 ms | UNVERIFIED (secondary) [R1 §F] |
| propagation delay | d / c (1 µs per 300 m) | physics |

### 4.5 Clear channel assessment and CBR

CBR = `T_busy / T_CBR`, busy when the received signal strength exceeds −85 dBm, `T_CBR` = 100 ms, alternative measurement methods within ±3 % [EN 302 571 §4.2.10.1 Eq. 1; EN 302 663 §4.3.2, R1 §A.6]. Simulator defaults elsewhere: ns-3 `RxSensitivity` −101 dBm (20 MHz reference), `CcaEdThreshold` −62 dBm, `CcaSensitivity` −82 dBm; Veins `ccaThreshold` −65 dBm [R1 §B.3]. Parameters: `cca_busy_dbm` default −85 dBm with source EN 302 571 §4.2.10.1 (the CBR busy threshold; CCA-ED and the CBR threshold are related but not defined as the same parameter by the standards, which the card states), presets `ns3` (−82 / −62) and `veins` (−65). `Mac::cbr` returns the last 100 ms window; the `high` tier measures it from the frame-level busy intervals, the `medium` tier from the slot occupancy, the `abstract` tier estimates it from local load (§4.9).

### 4.6 Frame overheads and size caps

| Item | Bytes | Source and status |
|---|---|---|
| MAC header, Data (no QoS) | 24 | standard 802.11 frame format; clause UNVERIFIED [R1 §B.3] |
| MAC header, QoS Data (used for BSM and CAM ACs) | 26 | clause UNVERIFIED |
| FCS | 4 | standard constant, not independently verified |
| LLC/SNAP (ITS-G5) | 8 (DSAP/SSAP 0xAA, control 0x03, OUI 00-00-00, EtherType) | DERIVED from 802.2/SNAP; EN 302 663 §4.3.1 Figure 3 shows "IEEE/ISO/IEC 8802-2 with SNAP" [R1 §B.3; R4 §F.2] |
| EtherType | 0x88DC (WSMP), 0x8947 (GeoNetworking) | Wireshark `etypes.h` [R4 §F.1-F.2], secondary |
| Maximum MSDU | 2,304 | well-established constant; clause UNVERIFIED [R1 §B.3; NDSS 2024 cites 802.11 Table 9-25] |
| Maximum MPDU | 2,346 | UNVERIFIED |
| NDSS frame accounting | MAC frame = 40 + SPDU | [NDSS 2024 §V-D, R4 §C.2], secondary |

Fragmentation: 802.11 fragments only individually addressed MSDUs; group-addressed frames (BSM, CAM, DENM) are never fragmented at the MAC; clause number UNVERIFIED but corroborated by two independent observations [R1 §B.3]. Consequently `Phy::begin_tx` rejects a frame above the MSDU cap and the `Fragmenter` of §7.3 must have acted first.

### 4.7 PER model

`phy/80211p/nist-per` (error model used by `Phy::finish_rx` in `medium` and `high`). Pei and Henderson's re-derivation of the ns-3 OFDM error model from Miller's NIST BER equations, validated within about 1 dB against the CMU wireless-emulation testbed (the older Yans model was 8-10 dB too optimistic) [Pei and Henderson 2010 §II-III, R1 §C.1].

```
BER:  BPSK  p = Q(sqrt(2·F·Eb/N0));   QPSK same form;   16-QAM p = (3/4)·Q(sqrt(F·(4/5)·Eb/N0));   64-QAM p = (7/12)·Q(sqrt(F·(6/21)·Eb/N0));   F = (4/5)·(48/52)
Coded: Chernoff-bound union sum over the convolutional code's distance spectrum (rates 1/2, 2/3, 3/4) gives P_e
PER = 1 − (1 − P_e,SIGNAL)^24 · (1 − P_e,DATA)^(N_SERVICE + 8·bytes + N_TAIL)
```

[Pei and Henderson 2010 Table I and Eq. 2, R1 §C.1; reproduced in the cached `nist_per.py`]. The BER formulas depend only on modulation and coding, so the 802.11a/g constants apply unchanged to the 10 MHz rates (derived reasoning, consistent with EN 302 663 Table C.1) [R1 §C.1]. SNR is the effective SINR at the receiver: `SINR = P_rx − 10·log10(N + Σ I_k)` with interferers summed in id order (02 §6.3) over the overlap windows (`high`) or over the whole frame (`medium`).

Model output (AWGN, ideal receiver; computed with `nist_per.py` from the cited model, not a measurement):

| MCS | SNR for PER 10 % at 200 / 400 / 1000 B (dB) | SNR for PER 1 % at 400 B (dB) |
|---|---|---|
| 3 Mbit/s BPSK 1/2 | 3.4 / 3.6 / 3.8 | 4.2 |
| 4.5 Mbit/s BPSK 3/4 | 6.2 / 6.5 / 6.7 | 7.2 |
| 6 Mbit/s QPSK 1/2 | 6.4 / 6.6 / 6.9 | 7.3 |
| 9 Mbit/s QPSK 3/4 | 9.3 / 9.5 / 9.7 | 10.2 |
| 12 Mbit/s 16-QAM 1/2 | 12.9 / 13.1 / 13.4 | 13.8 |
| 18 Mbit/s 16-QAM 3/4 | 16.0 / 16.2 / 16.5 | 16.9 |
| 24 Mbit/s 64-QAM 2/3 | 20.7 / 20.9 / 21.2 | 21.7 |
| 27 Mbit/s 64-QAM 3/4 | 21.9 / 22.2 / 22.5 | 23.0 |

Measured gap: Sjöberg et al. (Atheros 802.11p chipset, cabled, AWGN, 6 Mbit/s, noise floor about −110 dBm, SNR from RSSI not verified against a power meter) put 10 % PER at about 11.1 dB for 100 B, 11.5-11.7 dB for 300 B, 11.6-11.8 dB for 500 B, 11.7-11.9 dB for 723 B (digitized ±0.3 dB from Fig. 5) [Sjöberg et al., R1 §C.2]; no 3 Mbit/s measurement exists. The model predicts 6.6 dB at 400 B, so real hardware needs about 5 dB more [R1 §C.2]. Parameter `rx_impl_loss_db` (implementation loss added to the required SNR): default 0 dB (standards-ideal, source: model) with preset `sjoberg-atheros` = 5 dB (source: R1 §C.2 gap, `literature-checked`). Independent reference for fading channels: WiLabV2Xsim 11p curves give SINR at 10 % PER of −0.24 dB (MCS0 190 B, highway LOS), 3.10 dB (MCS0 190 B, highway NLOS), 3.86 dB (MCS2 350 B, urban LOS), 5.92 dB (MCS2 350 B, crossing NLOS), 6.78 dB (MCS2 350 B, highway NLOS) [WiLabV2Xsim PER tables, R2 §C], which the `high` tier (NIST + Nakagami) must bracket in §13.

### 4.8 Capture and hidden terminals

Capture rule (`high`): a frame is decodable if its power exceeds the cumulative interference plus noise by the capture threshold throughout reception, evaluated continuously, not only at arrival (`P_r ≥ I + CpTh`) [Torrent-Moreno 2009 Eq. 1, R1 §B.4]; reference threshold 5 dB for 3 Mbit/s BPSK 1/2 [same]. The engine implements capture the way Veins does: per symbol-group SINR windows over the arrival set and the error model above, so no separate scalar is needed; `CpTh` remains as an optional hard-threshold shortcut (`capture = {sinr-trace, threshold(CpTh)}`); ns-3 exposes a pluggable `FrameCaptureModel` with no fixed constant [R1 §B.4]. Preamble detection: an arrival can be locked only when the receiver is idle or the new preamble exceeds the current lock by `CpTh`; a receiver that is transmitting (half duplex) loses every arrival (`LossCause::HalfDuplex`). Hidden terminals are not a separate model: they emerge from CSMA with per-node CCA and the propagation model, and are reported as `LossCause::HiddenTerminal` when the colliding transmitter was outside the receiver's CCA range at the start of the frame.

### 4.9 Tiers, what each ignores, and abstract-tier calibration

| Tier | PHY | MAC | Ignores relative to the next tier |
|---|---|---|---|
| `abstract` (`phy/abstract/distance-load-table`) | reception = calibrated probability of (distance bin, local load bin) | none; load feeds the PHY | SINR, interference geometry, capture, timing, hidden terminals |
| `medium` (`phy/80211p/ofdm-10mhz` + `mac/80211p/slotted-abstraction`) | SINR with aggregate interference from all transmitters overlapping the frame, NIST PER | slotted CSMA: contention resolved per 13 µs slot with a collision when two nodes in CCA range pick the same slot; no AIFS per AC, no backoff freezing | overlap-window SINR, capture timing, preamble detection, per-AC AIFS, backoff freeze, 1609.4 switching |
| `high` (`phy/80211p/ofdm-10mhz` + `mac/80211p/edca-ocb`) | preamble detection, capture, symbol-group SINR windows, half duplex, Nakagami fading, implementation loss | full EDCA state machine, hidden terminals, optional 1609.4 | frequency selectivity, Doppler, real chipset quirks beyond `rx_impl_loss_db` |

Legacy abstract models, kept for parity: **`phy/abstract/disc-legacy`**: heard iff `d ≤ radio_range_m` (500 m), then dropped with probability `packet_loss_base + nlos_loss·(d/rr) + cong + wx_loss` where `cong = min(0.8, max(0, (load − chan_capacity)/chan_capacity)·0.5)`, `chan_capacity` 40 in-range messages per step, `packet_loss_base` 0, `nlos_loss` 0, `wx_loss` per §2.6 [`run.py` L2684-2752, code (legacy)]. **`phy/abstract/logdistance-legacy`**: heard iff `10·n·log10(rr/d) − margin + N(0, σ) ≥ 0` with `n` 2.7, σ 4.0 dB, margin 0 dB, candidate window capped at `rr · 10^((cap_sigma·σ − min(0, margin))/(10·n))` bounded by `radio_cap_max_mult` [`run.py` L2696-2733, code (legacy)]; then the same drop rule. These draw from per-link streams in the new engine (02 §6.2). Both are registered `uncalibrated`.

Calibration procedure for `phy/abstract/distance-load-table` (satisfies I-R4; the same procedure applies per RAT, so §5 refers back here):

1. Worlds and densities: `tr36885-freeway` at 15, 60 (urban grid) and 70, 140 km/h (freeway) drops with 2.5 s headway [TR 36.885, §2.4], plus Todisco densities 50, 100, 200 veh/km on the 2 km highway [R2d], plus the Bazzi Cologne and Bologna neighbor densities (14.8 ± 8.8 per 100 m and 25.4 ± 25.4 per 100 m) [Bazzi 2018, R2d] as the urban points. Message load: 10 Hz, 300 B (or the TR 37.885 pattern {300, 190, 190, 190, 190} B); MCS 6 Mbit/s; power 23 dBm.
2. Run the homogeneous `high` tier with the propagation stack the scenario will use (medium or high propagation) for N seeds (registry default 10) and record, for every (transmitter, receiver, frame), the distance and the receiver's local load (number of distinct transmitters heard in the last `T_CBR`, or the measured CBR).
3. Bin distance in 25 m bins from 0 to `range_max` (default 1,000 m) and load in bins of the registered load axis (CBR in steps of 0.1, or heard-transmitter counts in steps of 10); estimate `P_rx[d_bin][load_bin]` as the pooled reception ratio with its Wilson interval.
4. Register the table as `phy/abstract/distance-load-table@<hash>` with the world ids, densities, seeds, propagation stack id and engine build in its card; the abstract tier interpolates linearly in distance and load and draws one Bernoulli per (link, frame) from the `AbstractRx` stream.
5. Acceptance: re-run the abstract tier on the same scenarios; for each 25 m bin at each calibration density the abstract PDR must be within 5 percentage points of the high-tier PDR (the tolerance stated in 02 §7.3 and I-R4), and the mean CBR estimate within 0.05. A table that fails any bin is registered `uncalibrated` and the validator warns; the focus-region test (02 §7.3) reuses the same tolerance at the boundary.
6. Scope: a table is valid for the RAT, MCS, packet-size pattern and propagation stack it was built with; the validator rejects use outside that envelope (for example a 1,000 B CPM load on a 300 B table).

The procedure is deterministic given the seeds and is part of the nightly validation suite (§13).

## 5. PHY and MAC: LTE-V2X Mode 4 and NR-V2X Mode 2

Models: `phy/lte-v2x/mode4` and `mac/lte-v2x/sps-sensing` (RAT `LteV2xPc5`), `phy/nr-v2x/mode2` and `mac/nr-v2x/sps-sensing` (RAT `NrV2xPc5`), error models `phy/lte-v2x/bler-lut-r1-160284` and `phy/cv2x/bler-lut-wilab`. Mode 4 and Mode 2 share one parameterized sensing and SPS engine (sensing window, selection window, RSRP exclusion with 3 dB step-up until a candidate percentage survives, S-RSSI ranking, random pick, reservation with probabilistic keep); Mode 2 adds re-evaluation, pre-emption, PSFCH feedback and numerology [Garcia 2021 §II.B, §V.B; R2 modeling notes 1-2]. The `ResourceModel::SidelinkPool { subch, period, sps }` of 03 §4 carries the pool configuration.

### 5.1 LTE-V2X Mode 4 (Rel-14)

Resource structure [Garcia 2021 §II.A; Bazzi 2018 §II-A; Molina-Masegosa 2017; R2 §A, R2e]:

| Item | Value | Source |
|---|---|---|
| Bandwidth, TTI | 10 or 20 MHz; 1 ms subframe | Garcia 2021 §II.A; TR 36.885 Annex A.1.1 |
| Subframe | 14 OFDM symbols (normal CP): 9 data, 4 DMRS (symbols 3, 6, 9, 12), 1 guard (14th) | Garcia 2021 §II.A |
| RB | 12 subcarriers × 15 kHz = 180 kHz | Garcia 2021 §II.A |
| `sizeSubchannel-r14` | {n4, n5, n6, n8, n9, n10, n12, n15, n16, n18, n20, n25, n30, n48, n50, n72, n75, n96, n100} PRBs | TS 36.331 V14.4.0 ASN.1 (grep verified), R2 §A |
| PSCCH | 2 PRBs; adjacent (first 2 RBs of the sub-channel) or non-adjacent (separate SCI pool), a pool configuration | TS 36.213 V14.4.0 §14.2.4 |
| SCI format 1 | 32 bits: priority, resource reservation, frequency resource location (variable), time gap initial to retransmission 4 bits, retransmission index 1 bit, MCS, reserved zero-padded | TS 36.212 V14.4.0 §5.4.3.1.2 (verbatim); per-field widths beyond those quoted UNVERIFIED [R2e] |
| MCS | 0-28; QPSK and 16-QAM only in Rel-14 (clause UNVERIFIED) | R2e |
| Reference packet mapping | 190 B: QPSK r0.7 in 10 RBs (fits a 12-RB sub-channel with 2 RBs SCI); 300 B: QPSK r0.5 in 20 RBs (two 12-RB sub-channels, 22 allocated); 10 MHz = 50 RBs = 4 sub-channels of 12 RBs (the paper's own choice; 3GPP does not fix RBs per sub-channel) | Molina-Masegosa 2017 §III; R2 §A |
| Bazzi mapping | 300 B beacons at MCS 4 (1 beacon resource per TTI) or MCS 7 (2 per TTI) with 10-PRB sub-channels | Bazzi 2018 Table 2, R2e |
| Tx power | 23 dBm (33 dBm not precluded) | TR 36.885 Annex A.1.1 |
| Sensitivity, maximum input | −90.4 dBm, −22 dBm | TS 36.101 v14.4.0 via R2e |
| Noise figure, antenna | 9 dB; 3 dBi at 1.5 m (vehicle, UE-type RSU), 0 dBi (pedestrian), RSU 5 m | TR 36.885 Annex A.1.1 |
| In-band emissions | TS 36.101 §6.5.2A.3 mask with {W, X, Y, Z} = {3, 6, 3, 3} for single-cluster SC-FDMA (the parameterization reused by TR 36.885 Annex A.1.1); the numeric mask table itself UNVERIFIED (garbled extraction) | TR 36.885 Annex A.1.1; Bazzi 2018 Appendix A `K_IBE` |
| Half duplex | a UE cannot sense or receive in a subframe in which it transmits; those subframes are excluded from the sensing history (`q·RRI` rule) | Garcia 2021 §II.B; TR 36.885 Annex A.1 |

Sensing-based semi-persistent scheduling (`mac/lte-v2x/sps-sensing`), executed when a new TB arrives with no valid reservation or when the reselection counter expires [Garcia 2021 §II.B; TS 36.213 §14.1.1.6 (equations lost in conversion, corroborated by Bazzi 2018 Table 1 and Molina-Masegosa 2017); R2 §A; R2e]:

1. Sensing window: the last 1,000 subframes before the trigger subframe `n`.
2. Selection window `[n + T1, n + T2]`: `T1 ≤ 4` subframes (UE choice, used 1); `T2` in `[20, 100]`, or `[T2min, 100]` with `T2min` in [10, 20] by priority when configured; `T2` also bounded by the latency deadline (100 / 50 / 20 ms for 10 / 20 / 50 pps).
3. Exclude every candidate (sub-channel set × subframe) whose sensed SCI reservation overlaps it with RSRP above the threshold for the (Tx priority, Rx priority) pair from a 64-entry list spanning [−128, −2] dBm; the algebraic form `P_th = −128 + 2·index` is derived, not quoted [R2 §A]. Also exclude candidates the UE could not sense because it was transmitting.
4. If fewer than 20 % of candidates remain (`R_sel = 0.2`, mandated), raise the threshold by 3 dB and repeat.
5. From the survivors keep the 20 % with the lowest average S-RSSI measured over subframes `n − 100·j`, `j = 1..10` (for RRI 100 ms; `T·j` in general) [TS 36.214 §5.1.28 via R2e], and pick one uniformly at random (`SpsSelection` stream).
6. Reserve it every RRI for `C_resel` transmissions: `C_resel` uniform in [5, 15] for RRI ≥ 100 ms, [10, 30] for 50 ms, [25, 75] for 20 ms; RRI ∈ {0 (no reservation), 20, 50, 100, 200, ..., 1000} ms, up to 16 configured, 12 non-zero values defined [Garcia 2021 §II.B]. At expiry keep the resource with `probResourceKeep` ∈ {0, 0.2, 0.4, 0.6, 0.8} [TS 36.331 V14.4.0 `probResourceKeep-r14`, verified], else reselect. Simulation choices in the literature: 0.4 (Bazzi), 0 (Molina-Masegosa) [R2e]; default 0 (registered as a study choice).
7. HARQ: blind retransmission signaled by the SCI time-gap field; the common ceiling of one blind retransmission (two transmissions per TB) within ±15 ms is a simulator convention, not a verified spec maximum [R2 §A; R2e]; default 0 retransmissions, option 1.

Baseline RSRP thresholds used by published simulators: −110 dBm (Molina-Masegosa), −126 dBm (OpenCV2X), −128 dBm (Bazzi "used if not specified") [R2 §A]; default −110 dBm for the Molina-Masegosa validation profile, −128 dBm otherwise, both recorded as study choices.

Channel accounting [TS 36.214 §5.1.30-5.1.31 via Garcia 2021 §II.B and R2e]: CBR = fraction of sub-channels whose S-RSSI exceeds the configured threshold over the previous 100 subframes; CR at subframe `n` = (sub-channels used in `[n − a, n − 1]` + granted in `[n, n + b]`) / all configured sub-channels over the window, `a + b + 1 = 1000`, `a ≥ 500`. CR limits: `SL-CBR-CommonTxConfigList` with up to 16 CBR ranges mapping to `CRLimit` by priority (IE verified); the numeric table is defined regionally (ETSI TS 103 574 in Europe) and is UNVERIFIED here [R2 §A, modeling note 8]. An illustrative, non-normative table from a RAN1 contribution [Qualcomm R1-1611594 via Mansouri 2019 Table III, R2e]: CBR ≤ 0.65 no limit; 0.65-0.675: 1.6e−3; 0.675-0.70: 1.5e−3; 0.70-0.725: 1.4e−3; 0.725-0.75: 1.3e−3; 0.75-0.80: 1.2e−3; 0.80-0.825: 1.1e−3; 0.825-0.85: 1.0e−3; 0.85-0.875: 0.9e−3; above 0.875: 0.8e−3. It ships as preset `cr-limit-r1-1611594` tagged `illustrative`; congestion control levers when CR exceeds the limit are drop, rate reduction, or MCS and sub-channel reduction (§6).

Error models. **`phy/lte-v2x/bler-lut-r1-160284`**: verbatim BLER versus SNR lookup for 190 B at 280 km/h relative speed [Huawei R1-160284 via Gonzalez-Martin 2019 `get_BLER.m`, R2e]:

| MCS | SNR (dB) → BLER |
|---|---|
| QPSK r0.7 | 0 → 1; 2 → 0.9; 4 → 0.7; 6 → 0.4; 8 → 0.13; 10 → 0.045; 12 → 0.017; 14 → 0.007; 16, 18, 20 → 1e−3 |
| QPSK r0.5 | −2 → 1; 0 → 0.9; 2 → 0.7; 4 → 0.3; 6 → 0.09; 8 → 0.02; 10 → 0.002; 12, 14 → 1e−3 |

Interpolated SINR at 10 % BLER: about 8.7 dB (r0.7) and 5.9 dB (r0.5) (derived in R2e, not stated by a source). Hard-threshold alternative for 300 B: MCS 4 at 2.76 dB, MCS 7 at 7.30 dB [Bazzi 2018 Table 2, R2e]. Curves for MCS 10 and 20 and for 300 B at LTE numerology were not found (UNVERIFIED; plan: regenerate from a link-level simulator under the §3.3 channel models). **`phy/cv2x/bler-lut-wilab`**: the WiLabV2Xsim PER tables (26 valid of 45 files; the 19 "404: Not Found" placeholders must not be used) with SINR at 10 % PER, LTE rows [R2 §C]: highway LOS MCS3 190 B −0.53 dB, MCS4 350 B 0.14, MCS5 350 B 1.20, MCS7 190 B 8.71, MCS7 350 B 4.14, MCS9 350 B 10.54, MCS11 550 B 7.16; highway NLOS MCS3 190 B 2.35, MCS4 3.31, MCS5 4.37, MCS7 7.29, MCS9 14.63, MCS11 550 B 10.70; urban LOS MCS4 0.83, MCS5 1.87, MCS7 4.89, MCS9 11.55; crossing NLOS MCS4 2.85, MCS5 3.85, MCS7 6.88, MCS9 13.71. The card lists which (scenario, MCS, size) cells are backed by data and which are interpolated; the origin paper's own PDR figures (Bazzi 2019 Future Internet) are UNVERIFIED (fetch blocked).

Reception in `medium` and `high`: SINR per sub-channel with the interferer sum including in-band emissions from co-subframe transmitters on other sub-channels (`K_IBE` mask); half-duplex loss; SCI decoding first (SCI failure loses the TB, `LossCause::ResourceCollision` when the collision is a same-resource reservation); then the LUT draw.

### 5.2 NR-V2X Mode 2 (Rel-16)

Numerology and slot structure [Garcia 2021 Table IV, §V.B-V.C; R2b; R2 §B]:

| Item | Value | Source |
|---|---|---|
| Numerologies | µ = 0, 1, 2 in FR1: SCS 15 / 30 / 60 kHz, slot 1 / 0.5 / 0.25 ms, 14 symbols (12 with extended CP at 60 kHz); µ = 3 (120 kHz) FR2; one numerology per pool | TS 38.211 via Garcia 2021 Table IV |
| Slot layout | first symbol AGC (duplicate of the second); PSCCH 2 or 3 symbols from the second symbol on `M_PSCCH` PRBs (< sub-channel size); PSSCH from the second symbol to the second-to-last; guard symbol; PSFCH when configured = 1 symbol + 1 AGC + 1 guard, at most 9 PSSCH symbols then; 7-14 consecutive SL symbols per slot | Garcia 2021 §V.B, §V.C.1 |
| `sl-SubchannelSize-r16` | {n10, n12, n15, n20, n25, n50, n75, n100} PRBs | TS 38.331 ASN.1 (grep verified), R2 §B |
| PSCCH PRBs | {10, 12, 15, 20, 25}, below the sub-channel size | RAN1 #99 via Garcia 2021 (clause in 38.331 UNVERIFIED) [R2b] |
| Pool bitmap, periodicity | length 10-160; 10,240 ms | Garcia 2021 §V.A.3 |
| SCI 1-A | priority 3 b; frequency resource assignment `ceil(log2(N(N+1)/2))` or `ceil(log2(N(N+1)(2N+1)/6))` b (`sl-MaxNumPerReserve` 2 or 3); time resource 5 or 9 b; reservation period `ceil(log2 N_rrp)` b; DMRS pattern; 2nd-stage format 2 b; beta offset 2 b; DMRS ports 1 b; MCS 5 b; additional MCS table 0-2 b; PSFCH overhead 0-1 b; reserved bits; CRC 24 b; polar coded; QPSK. Total is configuration dependent (no fixed 32-bit equivalent) | TS 38.212 v16.6.0 §8.3.1.1 [R2b]; R2 §B |
| SCI 2-A | HARQ process 4, NDI 1, RV 2, source id 8, destination id 16, HARQ feedback enabled 1, cast type 2, CSI request 1 = 35 bits + 24 CRC | TS 38.212 §8.4.1.1 [R2b] |
| SCI 2-B | HARQ 4, NDI 1, RV 2, source 8, destination 16, HARQ enabled 1, zone id 12, communication range requirement 4 = 48 bits + 24 CRC | TS 38.212 §8.4.1.2 [R2b] |
| PSSCH | LDPC; QPSK to 256-QAM; 1-2 DMRS ports; MCS tables TS 38.214 Table 5.1.3.1-1 (64-QAM: MCS 0 Qm 2, R 120/1024, SE 0.2344; MCS 28 Qm 6, R 948/1024, SE 5.5547), -2 (256-QAM), -3 (low SE); `SL-MinMaxMCS-Config-r16 { sl-MCS-Table-r16 {qam64, qam256, qam64LowSE}, min 0..27, max 0..31 }` | TS 38.214 §5.1.3.1 (verified); TS 38.331 (verified) [R2 §B, R2b] |
| PSFCH | period 1, 2 or 4 slots; Zadoff-Chu (PUCCH format 0 based) on one PRB, CDM between UEs; HARQ for a PSSCH ending in slot `n` sent in slot `n + a`, `a` the smallest integer ≥ K with PSFCH, K ∈ {2, 3}; unicast ACK/NACK; groupcast option 1 NACK-only distance based with the range from a 16-entry list drawn from {20, 50, 80, 100, 120, 150, 180, 200, 220, 250, 270, 300, 320, 350, 370, 400, 420, 450, 480, 500, 550, 600, 700, 1000} m; option 2 ACK/NACK from all; broadcast no feedback | Garcia 2021 §V.B.4, §V.C.4 [R2b] |

Sensing and selection (`mac/nr-v2x/sps-sensing`) [TS 38.214 §8.1.4; TS 38.331 IEs verified; Garcia 2021 §VI.B; R2b; R2 §B]:

| Item | Value |
|---|---|
| Sensing window | `[n − T0, n − T_proc,0)`, `sl-SensingWindow-r16 {ms100, ms1100}` in slots by SCS |
| `T_proc,0` | 1, 1, 2, 4 slots for µ = 0..3 (Table 8.1.4-1) |
| `T_proc,1` | 3, 5, 9, 17 slots (3, 2.5, 2.25, 2.125 ms) (Table 8.1.4-2) |
| Selection window | `[n + T1, n + T2]`, `T1 ≤ T_proc,1`; `T2min ≤ T2 ≤ PDB`; `sl-SelectionWindow-r16 {n1, n5, n10, n20}` × 2^µ slots per priority 1..8 |
| Exclusion | RSRP per priority pair from `sl-ThresPSSCH-RSRP-List`, Rel-16 range (−112 + 2n) dBm, 0 ≤ n ≤ 45 (narrower than LTE's [−128, −2]; must not be conflated) |
| Candidate percentage | `sl-TxPercentage-r16 {p20, p35, p50}` per priority; below it, threshold + 3 dB and repeat |
| Re-evaluation | at slot `m − T3` (`T3 = T_proc,1`) before each selected resource is used; only the invalidated subset is reselected; new in Rel-16 |
| Pre-emption | a lower-priority UE frees a reserved resource for an estimated higher-priority user above `sl-PreemptionEnable` threshold; re-runs steps with window `T2min ≤ T2'' ≤ PDB − (n'' − n_G)`; per-pool enable |
| Reservation | `sl-ResourceReservePeriodList-r16` up to 16 periods from {0, 1..99 (integer ms), 100, 200, ..., 1000} ms; `Q = ceil(T2/RRI)` when `RRI < T2` and `n − s_i ≤ RRI`, else 1; `sl-MaxNumPerReserve-r16 {n2, n3}` resources per SCI within a 32-slot window; `N_MAX ≤ 32` selected resources, shrinkable by congestion control |
| Reselection counter | [5, 15] for RRI ≥ 100 ms; [5C, 15C] with `C = 100/max(20, RRI)` otherwise (TS 38.321 §5.22.1); `sl-ProbResourceKeep-r16 {0, 0.2, 0.4, 0.6, 0.8}` |
| HARQ | `sl-MaxTransNum-r16` INTEGER (1..32) total transmissions; blind when no PSFCH, minimum gap `t_GAP` between retransmissions when PSFCH is configured; the LTE ceiling of one blind retransmission does not apply |
| Non-sensing (random) selection | all in-pool resources in the window are candidates [Ali 2021 §II.1] |

Measurements [TS 38.215 §5.1.25-5.1.27, R2b; Garcia 2021 §VI]: SL RSSI = linear average received power over the sub-channel from the second symbol of a PSCCH/PSSCH slot; SL CR over `a + b + 1 = 1000·2^µ` slots with `b < (a + b + 1)/2`; SL CBR = fraction of sub-channels with SL RSSI above the threshold over `a = 100·2^µ` slots (threshold list (−112 + 2n) dBm, secondary); CR limit by up to 16 CBR ranges, TB priority and (Rel-16) absolute UE speed; the UE evaluates at slot `n − N_proc` with `N_proc` = 2 slots (µ = 0) or 2^µ / 2·2^µ slots by capability. The NR CR-limit table was not standardized at the time of the tutorial (UNVERIFIED; do not invent). Congestion levers: fewer sub-channels or lower MCS, smaller `L_PSSCH`, smaller `N_MAX`, lower power [R2b].

Simulation reference configurations [Ali 2021 Table I; Todisco 2021; R2 §B, R2d]: T0 100 ms, `T_proc,0` 2 slots, T1 2 slots, T2 17 / 33 / 65 slots for µ = 0 / 1 / 2 (fixed time) or 33 slots (fixed slot count), RSRP −128 dBm (a study value, outside the Rel-16 list range), sub-channel 50 RBs on 40 MHz, PSCCH 1 symbol, PSSCH 12 symbols, MCS Table 2 index 14 (PSSCH) and 0 (PSCCH), `N_PSSCH,maxTx` 5, `N_max,reserve` 3, keep probability 0; Todisco: 2 km highway, 3 lanes per direction, wrap-around, speed N(70, 7) km/h, 100 veh/km, 350 B every 100 ms (1,000 B CPM-like), 10 MHz, 13 dBm/MHz PSD, 3 dBi, NF 9 dB, WINNER+ B1 LOS, SCS 15 kHz, MCS 4 (QPSK, Rc 0.3). Numerology convention: the card must state whether `T2` is fixed in time or in slots across µ [Ali 2021 §III; R2 modeling note 2]. IBE matters: Todisco's SCS ablation shows the SCS benefit comes mainly from fewer co-slot IBE contributors [R2 modeling note 7], so the NR PHY carries an IBE model or over-predicts high numerologies.

### 5.3 3GPP evaluation assumptions

Beyond the channel models of §3.3: Uu ISD urban macro 500 m, highway 1,732 m (500 m optional); RSU spacing 50 or 100 m or one per intersection; macro BS Tx power 49 dBm below 6 GHz; BS noise figure 5 dB; aggregated bandwidth up to 200 MHz DL+UL and 100 MHz SL below 6 GHz [TR 37.885 Tables 6.1.1-1, 6.1.3-1; TR 38.913 Tables 6.1.8-1, 6.1.9-1, R11 §A4]; TR 36.885's own ISD table is UNVERIFIED (corrupted cache) but TR 37.885 cites its BS placement [R11 §A4]. Traffic Model 1: 100 ms period, sizes {300, 190, 190, 190, 190} B with random phase, latency 100 ms [TR 37.885 §6.1.5, R2 §D.2]. Metrics: PRR type 1 (by distance band) and 2 (intended receivers), PIR [TR 37.885 §6.1.6]. Headway: 2.5 s (TR 36.885) versus `max{2 m, Exp(2 s × speed)}` (TR 37.885) is a genuine revision; a scenario states which it uses [R2 modeling note 4]. Deployment context [R2 §F, R2d]: US C-V2X rules final (FCC 24-123) with DSRC sunset about December 2026; EU technology-neutral with ITS-G5 pilots and the VW Golf 8 shipping ITS-G5 (press sources); China MIIT 5905-5925 MHz for LTE-V2X PC5 (October 2018 plan; dates from search synthesis, re-check before relying on them); 5GAA device list April 2024.

### 5.4 Tiers

| Tier | PHY | MAC | Ignores relative to the next tier |
|---|---|---|---|
| `abstract` | `phy/abstract/distance-load-table` calibrated per RAT by the §4.9 procedure (load axis = CBR) | none | everything below |
| `medium` | SINR per sub-channel with aggregate interference over the whole subframe or slot, LUT draw, IBE included | SPS reservations honored as booked resources; sensing replaced by a collision probability from the pool occupancy; no RSRP exclusion details, no half duplex | half-duplex sensing gaps, RSRP threshold adaptation, re-evaluation and pre-emption, PSFCH timing |
| `high` | + half duplex, SCI decoding, symbol-group SINR, PSFCH | full sensing-based SPS with RSRP thresholds, reselection counters, keep probability; Mode 2 re-evaluation and pre-emption | link adaptation by CSI, MIMO, LDPC code-block segmentation (EESM effective SINR is approximated by the mean over the allocation) |

### 5.5 Validation targets

Digitized and quoted values that the `high` tier must reproduce (details of the check in §13) [R2d; R2 §E; R2 §D.1]:

| Source | Setup | Target |
|---|---|---|
| Molina-Masegosa and Gozálvez 2017 Fig. 3 | Highway Slow 120 veh/km at 70 km/h, 10 pps, 190 B (every fifth 300 B), 4 sub-channels of 12 RBs, WINNER+ B1, 23 dBm, NF 9 dB, RSRP −110 dBm | PDR ≈ 0.97 at 0 m, 0.95 at 100 m, 0.90 at 200 m, 0.79 at 300 m, 0.66 at 400 m, 0.45 at 500 m (digitized) |
| same, 50 pps | | ≈ 0.91 at 25 m, 0.58 at 100 m, 0.27 at 200 m, 0.11 at 300 m |
| same, Table 2 (exact) | Highway Slow: 10 pps 32.46 % sub-channels occupied, 3.38 % collisions; 50 pps 80.91 % / 56.64 %. Highway Fast (60 veh/km at 140 km/h): 10 pps 17.08 % / 0.78 %; 50 pps 62.08 % / 23.33 % | occupancy and collision ratios |
| same, text | 802.11p at 18 Mbit/s ≈ LTE-V up to about 160 m at 10 pps; at 250 m LOS about 20 % of TBs lost to collisions [R2 §E] | RAT comparison crossover |
| Bazzi 2018 Fig. 3 | Cologne (925 veh, 14.8 ± 8.8 neighbors per 100 m), Bologna (667 veh, 25.4 ± 25.4 per 100 m), highway (2,015 veh, 49.4 ± 12.5 per 200 m); 300 B at 10 Hz; WINNER+ B1 | average PRR within 100 m urban / 200 m highway: MCS 4: 0.63 / 0.58 / 0.66; MCS 7: 0.56 / 0.61 / 0.74; MCS 14: 0.50 / 0.60 / 0.70 |
| Todisco 2021 Fig. 7 | MCS 21, 350 B, no retransmission | worst (SCS 15 kHz with IBE) PRR ≈ 0.98 at 10 m, 0.89 at 60 m, 0.74 at 90 m, 0.62 at 100 m, 0.30 at 120 m, 0.04 at 150 m; best (SCS 60 kHz without IBE) ≈ 1.0 at 10 m, 0.85 at 90 m, 0.76 at 100 m, 0.46 at 120 m, 0.09 at 150 m |
| Todisco 2021 Fig. 8 | MCS 4, SCS 15 kHz, no retransmission | range at PRR 0.9: about 190 m at 50 veh/km, 155 m at 100 veh/km, 100 m at 200 veh/km |
| Todisco 2021 Fig. 10(a) | 350 B, RSRP −110 dBm | range 110 m without the L2 list, 170 m with `sl-TxPercentage` 20 % |
| TR 36.885 §9.1.1 Table 9.1-1 | RAN1 calibration | freeway PC5 about 80 % PRR at 320 m; urban 15 km/h 90 % at 50 m; urban 60 km/h about 60 % at 150 m |
| TR 38.913 §7.9 | 300 B | reliability 1 − 1e−5 at 3-10 ms user-plane latency (sidelink or BS-relayed) as a sanity bound |
| Gonzalez-Martin 2019 | analytical `PDR = 1 − (P_HD + P_SEN + P_PRO + P_COL)` | the engine's per-cause loss accounting (`LossCause`) must decompose the same way; numeric accuracy plots UNVERIFIED |

## 6. Congestion control

Models implement `Dcc` (03 §4): `on_cbr` receives the MAC's CBR each `T_CBR`, `gate` returns `Now{power, mcs}`, `DelayUntil(t)` or `Drop`, `state` is exported to the HUD. All are single-tier plug-ins (the tier is the MAC's); `abstract` runs use the same algorithms on the estimated CBR.

### 6.1 `dcc/etsi/adaptive-ts102687`

Adaptive approach [TS 102 687 V1.2.1 §5.4 Table 3, R1 §D.1]:

| Parameter | Value | Meaning |
|---|---|---|
| α | 0.016 | smoothing |
| β | 0.0012 | gain |
| `CBR_target` | 0.68 | target channel load |
| `δ_max` | 0.03 | upper bound on δ (from EN 302 571) |
| `δ_min` | 0.0006 | lower bound (anti-starvation) |
| `G_max+` | 0.0005 | upper clamp on the per-step offset |
| `G_max−` | −0.00025 | lower clamp |
| `T_CBR` | 100 ms | measurement interval; δ recomputed every 200 ms |

Every 200 ms (UTC mod 200 ms = 0): `CBR_ITS-S = 0.5·CBR_ITS-S + 0.5·(CBR_L_0_Hop + CBR_L_0_Hop_prev)/2` (or the global CBR from TS 102 636-4-2 sharing); `δ_offset = min(β·(CBR_target − CBR_ITS-S), G_max+)` if positive else `max(β·(CBR_target − CBR_ITS-S), G_max−)`; `δ = (1 − α)·δ + δ_offset`; clamp to `[δ_min, δ_max]` [TS 102 687 §5.4]. Gatekeeper (informative Annex B): next gate-open time `t_go = t_pg + min(max(T_on,pp/δ, 25 ms), 1 s)` [TS 102 687 Annex B, R1 §D.1]. Assumptions: CBR is measured by the access layer per §4.5; `T_on` known per queued packet from `Phy::air_time`. Validation target: steady-state CBR tracks 0.68 under increasing density (§13).

### 6.2 `dcc/etsi/reactive-ts102687`

State machine [TS 102 687 V1.2.1 Annex A Tables A.1 and A.2 (informative), R1 §D.2]:

| State | CBR | Table A.1 (`T_on` ≤ 1 ms): rate, `T_off` | Table A.2 (`T_on` ≤ 500 µs): rate, `T_off` |
|---|---|---|---|
| Relaxed | < 30 % | 10 Hz, 100 ms | 20 Hz, 50 ms |
| Active 1 | 30-39 % | 5 Hz, 200 ms | 10 Hz, 100 ms |
| Active 2 | 40-49 % | 2.5 Hz, 400 ms | 5 Hz, 200 ms |
| Active 3 | 50-60 % (A.1); 50-65 % (A.2) | 2 Hz, 500 ms | 4 Hz, 250 ms |
| Restrictive | > 60 % (A.1); > 65 % (A.2) | 1 Hz, 1,000 ms | 1 Hz, 1,000 ms |

The named states belong to TS 102 687 Annex A, not to TS 103 175 [R1 §D.2]. Hysteresis and state-hold timers are not specified in the cited tables; the model applies none by default (`hold_ms` 0, `TODO: calibrate`, plan: read the C2C-CC profile's DCC clauses in RS 2037 for the mandated hysteresis).

### 6.3 `dcc/etsi/cross-ts103175` and the EN 302 571 floor

Baseline limits that apply under any algorithm [EN 302 571 V2.1.1 §4.2.10.2 Eq. 2-5, R1 §D.3]: `0 < T_on ≤ 4 ms`, duty cycle ≤ 3 %; `T_off ≥ 25 ms` always; for CBR ≥ 0.62 also `T_off ≥ min{1000 ms, T_on·(4000·(CBR − 0.62)/(CBR − 1))}`. Cross-layer gatekeeping [TS 103 175 V1.1.1 §7.2 Eq. 1 (= TR 101 612 Eq. 7), REQ009, REQ022, REQ023, R1 §D.4]: congestion threshold `CTH` 0.62 (the same value passed to DCC_NET); `T_off,limit = (1/C_w)·T_on·(4000·(CBR − CTH)/(CBR − 1))`, negative means no restriction; `C_w` in (0, 1], default 1; enforced idle time `≥ min{1000 ms − T_on, T_off,limit}`. Worked examples (Table 2, `C_w` = 1): CBR 0.68 and `T_on` 1 ms → about 351.9 ms; CBR 0.75 → about 692.3 ms. Note the two targets: 0.62 (TS 103 175 CTH) and 0.68 (TS 102 687 adaptive) are independent tunables [R1 §D notes]. The engine enforces the EN 302 571 floor in `Dcc::gate` regardless of the selected algorithm, so a conformant `T_off,limit` bound holds for every plug-in.

### 6.4 `dcc/sae/j2945-1-rate-power`

SAE J2945/1 is paywalled; values are from Rostami, Krishnan and Gruteser 2018 (GM and Rutgers), which implements the standard's algorithm in a calibrated ns-3, cross-checked against the companion scalability paper [R1 §E; R4 §A.1]. The paper's symbol names are kept; the literal SAE names `vDensityWeightFactor`, `vRescheduleThreshold`, `vTxRand`, `vPERRange`, `vPERInterval`, `vPERMax`, `vPERSubInterval` were not found in any accessible source and stay UNVERIFIED.

Rate control (inter-transmission time) [Rostami 2018 Eq. 1-2, Table 1]:

| Parameter | Value |
|---|---|
| λ density smoothing weight | 0.5: `N_smooth(t) = λ·N(t) + (1 − λ)·N_smooth(t − 1)`, N = vehicles within 100 m |
| B density coefficient | 25 |
| `vMaxITT` | 600 ms |
| minimum ITT | 100 ms (`MaxITT = 100 ms` when `N_smooth ≤ B`, linear up to `vMaxITT` at `(vMaxITT/100)·B`, capped) |
| `vTxRateCntrlInt` | 100 ms |
| counting radius | 100 m |

Power control [Rostami 2018 Eq. 3-5, Table 1]: `vRPMax` 20 dBm, `vRPMin` 10 dBm, `vMinCU` 50 %, `vMaxCU` 80 %, `vSUPRAGain` 0.5, `vCBPMeasInt` 100 ms, initial `vRP` 15 dBm; `f(CBP) = vRPMax` for `CBP ≤ vMinCU`, linear to `vRPMin` at `vMaxCU`, `vRPMin` above; `RP(t) = RP(t−1) + vSUPRAGain·(f(CBP) − RP(t−1))`; `TxPower = RP − MinSectorAntGain + CLoss` (0 and 0 in the paper). Tracking-error trigger [Eq. 6]: `vTEMin` 0.2 m, `vTEMax` 0.5 m, `p(t) = 1 − exp(−α·(TE − vTEMin))` between them, 1 above `vTEMax`, 0 below; the sensitivity α is not given numerically (`TODO: calibrate`, plan: SAE J2945/1 §6 primary text). Certificate attachment `CertAttachInt` 450 ms [Rostami 2018 Table 1, twice independently]; "on new neighbor" trigger UNVERIFIED for SAE (it is verified for ETSI, §9.5). Position accuracy requirement: 1.5 m at 68 % via NHTSA (§3.8), UNVERIFIED at the SAE primary level.

### 6.5 Interaction with the message generators

`MessageGenerator::on_tick` receives `DccState` (03 §6). CAM: `T_GenCam_Dcc` is the DCC-provided minimum interval on ITS-G5, clamped to `[T_GenCamMin, T_GenCamMax]`; on LTE-V2X "DCC and `T_GenCam_Dcc` are not applicable" and congestion control lives in the access layer (TS 103 574) [EN 302 637-2 V1.4.1 §6.1.3, R4 §A.2]; C2C-CC sets `T_GenCam_Dcc = T_off` [RS 2037 RS_BSP_293]. VAM: `T_GenVam` from the VBS management entity clamped to `[100, 5000]` ms [TS 103 300-3 §6.2]. CPM: `T_GenCpm` within `[100, 1000]` ms (RSU down to 50 ms) [TS 103 324 Table F.1]. BSM: the J2945/1 ITT replaces the 100 ms nominal period and the rate controller and generator share the density estimate. Sidelink: the MAC's CR-limit enforcement (§5.1) can drop or delay; `Dcc::gate` returns `Drop` so the generator sees the loss, and the `node.tx` record carries the DCC state.

### 6.6 Ignores

None of the DCC models includes DCC_NET (GeoNetworking CBR sharing, TS 102 636-4-2) or the facilities-layer DCC_FAC in `abstract` and `medium`; `high` adds one-hop CBR sharing as an option (`cbr_sharing = true`) whose field structure is out of scope here. No model exists for the ETSI TS 103 574 LTE-V2X congestion table (UNVERIFIED numbers, §5.1).

## 7. Network, transport, fragmentation

`NetLayer` models return exact header sizes (`header_bytes`), never fragment (neither WSMP nor GeoNetworking has a fragmentation field), and expose the MTU; `Fragmenter` models act above them (03 §5). Byte accounting: every byte on the air is attributed to one bucket (I-N1).

### 7.1 `net/wsmp/1609-3`

WSMP v3 (IEEE 1609.3-2016/2020) layout [Wireshark `packet-wsmp.c`; IEEE 1609.3 ASN.1 `wsm.asn`, `wee.asn`; IEEE PSID tutorial; R4 §F.1] (all secondary or verified from the ASN.1; the standard itself is paywalled):

| Field | Bytes | Notes |
|---|---|---|
| WSMP-N-Header first octet | 1 | subtype 4 bits, option indicator 1 bit, version 3 bits |
| N-Header extensions (optional) | 3 each for one-byte values | channel number (id 15), data rate (id 16), transmit power (id 4); element id 1 + length 1-2 + value |
| TPID | 1 | PSID-only versus port-carrying transport PDU |
| WSMP-T PSID | 1-4 | p-encoded `VarLengthNumber`, values up to 0x1020407F |
| WSMP-T Length | 1-2 | variable |
| Minimum total | 4 | BSM with PSID 0x20 and payload ≥ 128 B: 5; with the three extensions: 14 (DERIVED) |
| "minimum 5, rarely exceeding 20" | | UNVERIFIED (search snippet) |
| EtherType | 0x88DC | Wireshark `etypes.h` |
| WSM maximum | 1,400 B default MIB payload; 2,302 B supported; MSDU cap 2,304 B; C-V2X MAP up to 8,000 B including security | CTI 4501 §4.3.3.1.3.1; NDSS 2024 [R4 §F.1, §B] |

Default `header_bytes` for a BSM: 5 (N-header 1 + TPID 1 + PSID 1 + length 2), plus 3 per requested extension. Alternatives in 1609.3 (UDP/IPv6, TCP/IPv6) are not modeled in Phase 1-2.

### 7.2 `net/gn-btp/en302636`

GeoNetworking headers [EN 302 636-4-1 V1.4.1 §9.5-9.7, Tables 11-17, Annex H, R4 §F.2] and BTP [EN 302 636-5-1 V2.2.1 §7.2-7.3, R4 §F.3], all VERIFIED:

| Header | Octets | Composition |
|---|---|---|
| Basic Header | 4 | |
| Common Header | 8 | |
| Long Position Vector | 24 | |
| Short Position Vector | 20 | |
| SHB (CAM, CPM, VAM) | 40 | 4 + 8 + SO PV 24 + media-dependent 4 |
| TSB | 40 | 4 + 8 + SN 2 + reserved 2 + LPV 24 |
| GBC / GAC (DENM) | 56 | 4 + 8 + SN 2 + reserved 2 + LPV 24 + lat 4 + long 4 + dist-a 2 + dist-b 2 + angle 2 + reserved 2 |
| GUC | 60 | 4 + 8 + SN 2 + reserved 2 + LPV 24 + SPV 20 |
| BEACON | 36 | 4 + 8 + LPV 24 |
| BTP-A (interactive) | 4 | destination port 2 + source port 2 |
| BTP-B (non-interactive, default) | 4 | destination port 2 + destination port info 2 |
| LLC/SNAP | 8 | DERIVED (§4.6) |
| Below a secured CAM | 52 | LLC/SNAP 8 + SHB 40 + BTP-B 4 (DERIVED) |

MTU rule: `MTU_GN ≤ MTU_AL − GEO_MAX` [§9.2.3]; `itsGnMaxGeoNetworkingHeaderSize` 88 (GUC with security accounting) and `itsGnMaxSduSize` 1,398 = 1,500 − 88 − 0 [Annex H items 8-9]. Well-known ports: SPATEM 2004, MAPEM 2003, IVIM 2006, SREM 2007, RTCMEM 2013 [TS 103 301 CSP_PortNo]; CAM 2001, DENM 2002, SSEM 2008 per TS 103 248 (UNVERIFIED, not extracted). Fragmentation: zero occurrences of "fragment" in EN 302 636-4-1 V1.4.1; oversize is handled at the facilities layer (MAPEM `layerID`, CPM segmentation) [R4 §F.2]. GN forwarding (GBC multi-hop, KAF for DENM) belongs to the `high` net tier; `abstract` counts headers only, `medium` adds reassembly (02 §7.1).

### 7.3 Fragmenter models

| Model id | Mechanism | Parameters and sources |
|---|---|---|
| `fragmenter/none` | oversize SDUs are rejected (`DropCause::Mtu`) | none; the default for BSM and CAM |
| `fragmenter/facilities-segmentation` | MAPEM: the RLT service fragments at the facilities layer when the size exceeds the allowed length, fragments identified by `layerID` (ISO/TS 19091 Annex G) [TS 103 301 §6.4.1]; CPM: the CPS assembles independently interpretable segments with `messageSegmentInfo` when more than one CPM is generated per event [TS 103 324 §6.1.2.1, §7.1.x]; each segment is a complete, separately signed message, so loss of one segment loses only its objects | CPM size limit `MTU_CPM = MTU_AL − HD_CPM − HD_NT` [TS 103 324 §6.1.3.1]; ETSI simulation segmentation threshold 1,100 B [TR 103 562 §5.5]; GN maximum packet lifetime for CPM ≤ 1,000 ms [TS 103 324 §5.3.3] |
| `fragmenter/cert-cycle-partial-hybrid` | Partially-Hybrid PQ design: BSMs keep ECDSA signatures; the hybrid certificate (ECDSA certificate + PQ signature over the ECDSA key, size 30 + pk + sig) is split into α equal fragments carried in the first α SPDUs of each τ = 5 SPDU (500 ms) certificate cycle; the remaining SPDUs carry the certificate hash; P2PCD learning responses are split into β fragments each sent after a uniform 0-250 ms wait (mean 125 ms) | [NDSS 2024 §IV, R4 §E; R5 §B.6]. Anchors: ECDSA certificate 162 B; Falcon-512 hybrid certificate 858 B gives α = 1, first frame payload 1,026 B, the other four 204 B each; learning-response times Falcon 250 ms, XMSS 375 ms, Dilithium about 500 ms, SPHINCS+ about 1,000 ms (8 fragments); frame sizes signed BSM + overhead + pk + sig: ECDSA 350, Falcon 2,435, XMSS 5,610, Dilithium 6,310, SPHINCS+ 15,844 B; caps 2,304 B (DSRC) and 437 B (C-V2X 10 MHz at the practical MCS, 3GPP Table A.8.3-1 via NDSS) or 2,481 B at MCS 11 with 10 sub-channels (J3161 via arXiv 2608.05087, R5); added per-BSM delay 0.25-0.39 ms |
| `fragmenter/generic-sdu` | split any SDU into `ceil(L/MTU_eff)` fragments with a 4-byte fragment header (id 2 + index 1 + count 1, a design choice recorded in the card); reassembly buffer per (sender digest, sdu id) with timeout; out-of-order tolerated | `reassembly_timeout_ms` default 1,000 ms anchored on the GN maximum packet lifetime for CPM [TS 103 324 §5.3.3] and marked `TODO: calibrate` (plan: sweep 250-2,000 ms against the P2PCD response-time anchors above and keep the smallest value that does not increase failed reassemblies at 60 veh/km) |

Every card states the timeout and the amplification formula (I-N2), and the conformance kit checks in-order reassembly and the reported amplification (03 §17).

### 7.4 Loss amplification

For an SDU carried in `n` fragments that are lost independently with probability `p` each, `P_sdu = 1 − (1 − p)^n`. Assumptions: independence between fragments (violated when fragments share a fading state or a burst of interference, which makes the true loss lower than the formula), equal `p` for every fragment (violated when fragments differ in size, so the model uses the per-fragment `p_i` from the PHY and `P_sdu = 1 − Π(1 − p_i)`), no retransmission (true for group-addressed frames), and a reassembly timeout longer than the spread of fragment arrivals (otherwise timeouts add to the loss). Example from the anchors: at `p` = 0.1 a five-fragment certificate cycle loses the certificate in 41 % of cycles; the Partially-Hybrid design's α = 1 for Falcon keeps `n` = 1 for the certificate and `n` = β for learning responses. Metric providers report `P_sdu` measured against the formula so that correlation effects are visible.

### 7.5 Tiers

`abstract`: header sizes only, no fragmentation state (an oversize SDU is counted as `n` frames with independent draws); `medium`: reassembly with timeouts and the amplification accounting; `high`: per-fragment timers, P2PCD interaction (§9.5), GN forwarding and KAF. Ignores are the complements.

## 8. Message sets, generation rules, codecs

### 8.1 Generation rules

Each row is a `MessageGenerator` model; defaults are the standard's values.

| Model id | Rule | Parameters (default, source, status) |
|---|---|---|
| `generator/bsm-j2945-1` | one BSM at least every 100 ms (10 Hz), ITT from §6.4 under congestion; Part I every BSM; Part II `VehicleSafetyExtensions` with PathHistory (about 300 m, max 23 points) and PathPrediction every BSM; events and lights conditional | 10 Hz VERIFIED (secondary: NDSS 2024 §II, Bindel 2021, Rostami 2018); `vMaxITT` 600 ms, `vMinITT` 100 ms (implied, UNVERIFIED); Part II cadence and PathHistory limits UNVERIFIED (patent text only) [R4 §A.1]; EEBL event flag at deceleration ≥ 0.4 g (§11) |
| `generator/cam-en302637-2` | check every `T_CheckCamGen ≤ T_GenCamMin`; trigger 1 (dynamics, after ≥ `T_GenCam_Dcc`): heading change > 4°, position change > 4 m, or speed change > 0.5 m/s since the last CAM; trigger 2 (periodic): elapsed ≥ `T_GenCam` (and ≥ `T_GenCam_Dcc` on ITS-G5); `T_GenCam` defaults to `T_GenCamMax`, becomes the elapsed time after a trigger-1 CAM, and resets to `T_GenCamMax` after `N_GenCam` consecutive trigger-2 CAMs; low-frequency container in the first CAM and then every ≥ 500 ms; special-vehicle container every ≥ 500 ms; RSU CAM ≥ 1,000 ms; generation budget < 50 ms | `T_GenCamMin` 100 ms, `T_GenCamMax` 1,000 ms, `N_GenCam` 3, all VERIFIED [EN 302 637-2 V1.4.1 §6.1.3-6.1.5, R4 §A.2]; C2C-CC: `T_GenCam_Dcc = T_off`, `N_GenCam = pCamGenNumber` [RS 2037 RS_BSP_293, RS_BSP_297]; field mean CAM interval 0.33-0.47 s [C2C-CC TR 2052 Obs. 10] as the validation target |
| `generator/denm-en302637-3` | new DENM gets an unused `actionID`; update increments `referenceTime`; repetition only when the application supplies `repetitionInterval` and `repetitionDuration` (neither is carried in the DENM); termination by cancellation (originator) or negation (other ITS-S), transmitted at least once; `T_Repetition = repetitionInterval`, bounded by `validityDuration`; default validity 600 s from `detectionTime`; KAF forwarding with `T_Forwarding = 2 × transmissionInterval + U(0, 150 ms)` capped at validity | VERIFIED [EN 302 637-3 V1.3.1 §6.1.2, §6.1.4.2, §8.2.1.5, §8.3.2.5, Annex B, R4 §A.3]; signer always certificate, `generationLocation` present [TS 103 097 §7.1.2]. Legacy trigger kept as preset `legacy-brake` (`code (legacy)`): benign DENM when speed ≤ 4.0 m/s after a deceleration ≥ 2.5 m/s², rate `denm_rate` per 100 s (0 default), fake-hazard fallback 40 per 100 s [`run.py` L106-121] |
| `generator/spat-map` | EU: SPATEM application-triggered and not repeated, port 2004, `CSP_MaxLat` 100 ms; MAPEM re-broadcast continuously with the SPATEM, port 2003; IVIM repeated at an application interval, port 2006; SREM triggered, may be repeated, port 2007; SSEM response, port 2008 (UNVERIFIED); RTCMEM not repeated, port 2013. US: SPaT average 10 per s ± 1 over 10 s, TSC to RSU never more than 0.3 s apart, latency ≤ 300 ms; MAP average 1 per s ± 1 over 10 s; RTCM MSM4 1-10 Hz; J2735_202007 UPER | [TS 103 301 V2.1.1 §5.4.2, §6.4.2-6.4.3, §7.4.2, §8.4.2, §9.4.2, Tables 3 and 8; CTI 4501 v01.01 §3.3.3.1.5.1-3, §3.3.2.1.1.3, Annex C.1, R4 §A.4]; ISO/TS 19091 rates UNVERIFIED (paywalled). Defaults: SPaT 10 Hz, MAP 1 Hz |
| `generator/vam-ts103300-3` | `T_GenVamMin` 100 ms, `T_GenVamMax` 5,000 ms, `T_AssembleVAM` 50 ms; low-frequency container first, then every ≥ 2,000 ms; triggers: elapsed > `T_GenVamMax`, position change > 4 m, speed change > 0.5 m/s, velocity-orientation change > 4°, trajectory-interception probability change > 10 %; redundancy mitigation skips an individual VAM when elapsed ≤ `numSkipVamsForRedundancyMitigation` (2-10, e.g. 4) × `T_GenVamMax` and a peer already reports within the thresholds, or when clustered or in a protected zone; cluster distance change threshold 2 m; certificate every ≥ 1 s or on a new CAM signer (individual), every ≥ 500 ms (cluster) | all VERIFIED [TS 103 300-3 V2.2.1 §6.2, §6.4.1, §6.4.3, §6.5.3, Tables 16-17, R4 §A.6]; V2.3.1 (2025-12) not checked |
| `generator/cpm-ts103324` | periodic every `T_GenCpm` in [100 (RSU 50), 1,000] ms, 0..n CPMs per event; sensor information every `T_AddSensorInformation` 1,000 ms; up to 8 perception regions; object inclusion (`ObjectInclusionConfig` 1): Type A (VRU, animal, group, other) when first detected, and all Type A when any was omitted for ≥ `T_GenCpmMax`/2; Type B (vehicles, motorcycles) when new, or position > 4 m, speed > 0.5 m/s, orientation > 4°, or ≥ `T_GenCpmMax` since inclusion; `ObjectPerceptionQualityThreshold` 3; UPER size ≤ `MTU_CPM`; segmentation per §7.3 | all VERIFIED [TS 103 324 V2.1.1 §6.1.2, §6.1.3.1, Annex F Table F.1, R4 §A.7] |
| `generator/wsa-1609-3` | `SrvAdvMsg { version, body }`, `RepeatRate` INTEGER (0..255) | ASN.1 VERIFIED [IEEE 1609.3 `wsa.asn`, `wee.asn`]; semantics "transmissions per 5 s" UNVERIFIED [R4 §A.8]; default 1 per 5 s marked UNVERIFIED |
| `generator/psm-j2945-9` | PSM for VRU devices over DSRC/1609 | scope only VERIFIED (secondary); rate rules UNVERIFIED (a patent claims 2-5 per s by speed; PASS used 100 ms) [R4 §A.5]; default 1 Hz `TODO: calibrate` (plan: SAE J2945/9 §6 primary text) |

### 8.2 Sizes

Sizes of encoded messages, with layer scope and status [R4 §B; R4 §C.2; R5 §A.1]:

| Message or element | Bytes | Scope | Source | Status |
|---|---|---|---|---|
| BSM Part I only (UPER) | about 39 | payload | none accessible | UNVERIFIED |
| BSM SPDU, digest signer | 180 | BSM + 1609.2 envelope | Rostami 2018 Table 1 | VERIFIED (secondary) |
| BSM SPDU, full implicit certificate | 250 | as above | Rostami 2018 Table 1 | VERIFIED (secondary) |
| BSM SPDU maximum | ≤ 226 implicit, ≤ 330 explicit | SPDU | NDSS 2024 §II-A, §IV-B | VERIFIED (secondary) |
| BSM span used by ePrint 2022/133 | 122-481 (payload 50-300 + 117 B certificate + 64 B signature; 8 B digest in 4 of 5) | SPDU | Cominetti 2022 citing VSC-A | VERIFIED (secondary) |
| BSM, V2Verifier testbed | 250 (BSM + signature), 530 with explicit certificate | testbed framing | Bindel 2021 | VERIFIED (secondary) |
| Signed BSM with full certificate, ECDSA P-256 / Falcon-512 | 301 / 1,735 | SPDU | arXiv 2608.05087 Tables 1-2 [R5 §A.1] | secondary |
| BSM planning size (US) | 380 | with security and higher layers | C2C-CC TR 2050 Fig. 17 | planning |
| CAM, field (ITS-G5, 2018) | mean 297-406 per drive, overall 357; min 182 (Renault) / 199 (VW); max 500-807; 30 % < 300, > 50 % > 350, > 30 % > 450; 63-76 % carried a certificate | secured CAM as captured; GN/BTP inclusion not stated | C2C-CC TR 2052 Tables 6-1, 6-2, Obs. 4-6 | VERIFIED (layer scope UNVERIFIED) |
| CAM theoretical range | 200-800 | | TR 2052 §2 | VERIFIED |
| CAM low-frequency elements | vehicle role 1, exterior lights 1, path history 8-9 per entry | | TR 2052 §3.1 | VERIFIED |
| CAM planning size (EU) | 400 | with security and GN | TR 2050 Annex A Fig. 10 | planning |
| DENM typical | about 300 (100-800+) | | unreviewed GitHub doc | UNVERIFIED |
| SPaT typical | none found | | | UNVERIFIED |
| SPaT + MAP planning (EU) | 1,200 | with security | TR 2050 Fig. 14 | planning |
| MAP ceiling (US) | < 2,302 with signature, certificate, header; WSM default 1,400; C-V2X 8,000 | | CTI 4501 §4.3.3.1.3.1 | VERIFIED |
| CPM containers (mandatory DEs) | header + management + station data 121; per sensor information 35; per perceived object 35; with optional DFs station data about 16 and about 31 per object | UPER | TR 103 562 Table 3, §5.3.2 | VERIFIED |
| CPM planning (EU) | 1,000 with security (750 B payload ≈ 25 objects) | | TR 2050 Fig. 13 | planning |
| VAM planning | 350 (TR 2050); 300 used by Ostendorf 2025 as midpoint of 235 (C2C-CC) and 350 (5GAA) | | TR 2050 Fig. 12; arXiv 2506.22052 | planning |
| PSM | none | | | UNVERIFIED |
| MCM / PCM planning | 1,000 / 400 | with security | TR 2050 Figs. 15-16 | planning |

### 8.3 Codec decision

`codec/uper/rasn-etsi` and `codec/coer/rasn-1609-2` (`MessageCodec`, tier `uper`): real ASN.1 encoding with `rasn` (MIT OR Apache-2.0; BER, DER, APER, UPER, JER, OER, COER, XER; `#[no_std]`) and `rasn-compiler` bindings generated from the ETSI forge modules (BSD-3-Clause: CDD TS 102 894-2, CAM EN 302 637-2, DENM EN 302 637-3, VAM TS 103 300-3, CPM TS 103 324, TS 103 301, TS 103 097, IEEE 1609.2 mirror, MRS TS 103 759) and `rasn-its` 0.28.14 for IEEE 1609.2-2022 and TS 103 097 [R4 §G]. Facilities messages are UPER; the security envelope and certificates are COER [TS 103 097 V2.1.1 §4.1; TS 103 324 §6.1.3.1]. Rejected: asn1c (BSD-2, C, parametrized-type issues), pycrate (LGPL, Python only), asn1tools (no CLASS or parametrization support) [R4 §G]; rasn has no documented V2X deployment (track record UNVERIFIED), which the conformance tests against ETSI test vectors mitigate.

`codec/size-model/j2735` (tier `size-model`): SAE J2735 ASN.1 is sold separately and may not be redistributed ("Redistribution of the ASN files is not permitted", USDOT asn1_codec) [R4 §G], so the repository ships a validated size model for BSM, SPaT, MAP, PSM, SRM, SSM and a build-time importer: a user who owns the module drops it into `third_party/sae/` and `cargo build --features j2735-real` generates real UPER bindings with `rasn-compiler`; without it the codec returns `Encoded { size_source: SizeModel(version) }` placeholders of exact modeled size. The J2735 2023 versus 2020 compatibility claim is UNVERIFIED.

### 8.4 Size model and tolerances

The size model is a table of (message type, content profile) → bytes with, for variable containers, a per-element increment (CPM 35 B per object and per sensor; CAM path history 8-9 B per entry). Validation and tolerance (I-S2): for each message the card records the anchors it is checked against and the anchor spread; a model value must fall inside the spread of the cited anchors for that message, and the tolerance recorded in the card is that spread (for example an implicit-certificate BSM SPDU: anchors 226 (NDSS maximum) and 250 (Rostami), so 226-250; a digest BSM SPDU: 180 (Rostami) versus the 93 B envelope + Part I estimate; a secured CAM: TR 2052 minimum 182-199 with digest and mean 357 with the certificate mix). Where only one anchor exists the tolerance is 0 and the status is `literature-checked (single anchor)`; where none exists (PSM, SPaT, DENM typical) the model value is `TODO: calibrate` with the plan "encode with the real module once imported, or with asn1c and the SAE module on a machine that holds a license, and record the measured bytes". When the real codec is available the conformance kit asserts `size_model == uper_len` for every generated message (I-S3 for envelopes), and any deviation fails registration.

## 9. Security envelope and primitives

### 9.1 Envelope overhead

`envelope/ieee-1609-2` and `envelope/etsi-ts103097` (`SecurityEnvelope`; the ETSI profile is `Ieee1609Dot2Data` with ETSI constraints, no separate trailer [TS 103 097 V2.1.1 §5.1-5.2]). COER sizes DERIVED from the ASN.1 [R4 §C.2]:

| Component | Bytes | Derivation |
|---|---|---|
| `Ieee1609Dot2Data` outer | 2 | protocolVersion 1 + content choice tag 1 |
| `hashId` | 1 | |
| `SignedDataPayload` preamble + inner `Ieee1609Dot2Data` | 1 + (1 + 1 + 1-2 length) | |
| `HeaderInfo` preamble + `psid` + `generationTime` | 1 + 2 (PSID 0x20; ITS-AIDs ≥ 256 take 3) + 8 (Time64) | `generationTime` always present in ETSI |
| `generationLocation` (DENM) | 10 | Latitude 4 + Longitude 4 + Elevation 2 |
| signer = digest | 9 | choice 1 + HashedId8 8 |
| signer = certificate | 3 + cert | choice 1 + SequenceOfCertificate quantity 2 + certificate |
| ECDSA P-256 signature | 66 | Signature choice 1 + rSig (EccP256CurvePoint choice 1 + 32) + sSig 32 |
| **Total with digest** | **≈ 93-94** | |
| **Total with certificate** | **≈ 87 + cert** | |

**MEASURED 2026-09-18** against the real COER encoder in `v2xw-sec` (`crates/v2xw-sec/tests/overhead.rs`), and the derivation above is confirmed line for line: overhead is **exactly 93 B** with a digest signer and PSID 0x20, **exactly 94 B** with a PSID of 128 or more (which is the 94 in the "93-94" range — the third PSID byte, and nothing else), and **exactly 87 B + the certificate** with a certificate signer. `generationLocation` costs exactly 10 B. The only variation beyond those is the COER length determinant of the inner `unsecuredData`: 93 B for a payload below 128 B, 94 B from 128 to 255, 95 B from 256. Invariant I-S3 therefore holds as equality, not as a tolerance.

HashedId8 and HashedId3 are the low-order 8 and 3 bytes of SHA-256 over the canonical COER encoding (SHA-256("") gives a495991b7852b855 and 52b855) [IEEE 1609.2a-2017 §6.3.25-6.3.26]; the Time64 epoch (2004-01-01 TAI µs) wording is UNVERIFIED. NDSS accounting for cross-checks: certificate = 30 + pk + sig; SPDU = 24 + BSM + cert + sig; MAC frame = 40 + SPDU [NDSS 2024 §V-D]. The prior "about 150 with digest / about 280 with certificate" appears in no source (UNVERIFIED, not used).

### 9.2 Certificate sizes

| Certificate | Bytes | Derivation or source | Status |
|---|---|---|---|
| Implicit (ECQV) pseudonym, 1609.2 | ≈ 80 | preamble 1, version 1, type 1, issuer 9, toBeSigned {preamble 1, id linkageData 13, cracaId 3, crlSeries 2, validityPeriod 7, appPermissions 8, verifyKeyIndicator 34}; no signature | DERIVED; cross-checks: Rostami 250 − 180 = 70 B delta → ≈ 76-80; VSC-A 117; NDSS SPDU ≤ 226 |
| Explicit, 1609.2 | ≈ 147 (DERIVED) / 162 (NDSS: 30 + pk + sig) | implicit − 34 + 35 (verificationKey) + 66 (signature) | DERIVED / VERIFIED (secondary); one-certificate P2PCD learning response 172 B |
| ETSI Authorization Ticket | 90-130 (DERIVED: `CertificateId` none, 2 PsidSsp, validity, region); 100-150 "certificates and signatures" field | TS 103 097 §7.2.1; C2C-CC TR 2052 §3.2 | DERIVED / VERIFIED (range) |
| ECQV reconstruction value | 33 (compressed point) | SEC 4 §3.4-3.5 with SEC 1 §2.3.3 | VERIFIED |

**MEASURED 2026-09-18** against the real COER encoder (`crates/v2xw-sec/src/cert.rs`, `the_derivation_reconciles_field_by_field`): an implicit pseudonym certificate of exactly the shape derived above is **77 B** and its explicit counterpart **144 B**, three bytes under each derived figure. Every component of the derivation is confirmed by the encoder — issuer 9, `linkageData` 13, `cracaId` 3, `crlSeries` 2, `validityPeriod` 7, reconstruction value 34, verification key 35, signature 66 — except **`appPermissions`, which the derivation gives as 8 B and which encodes in 5**: `SEQUENCE OF` quantity 2 (a one-byte length determinant plus the count) + `PsidSsp` preamble 1 + `psid` 2 (an unbounded INTEGER: one length byte, one value byte). Eight is what the same field costs with a service-specific permission present or a PSID of 128 or more, so the derivation is right about the shape and generous by three bytes about the one-`PsidSsp`, PSID-0x20 instance. The derived figures stay in the table as the conservative ones; the measured figures are what the simulator emits, and the difference between the explicit and the implicit form — 67 B, the whole point of ECQV — is exactly as derived.

### 9.3 Signed message totals

| Message | Digest | Certificate | Sources |
|---|---|---|---|
| BSM SPDU | 180 | 250 (implicit) | Rostami 2018 Table 1 (secondary) |
| BSM SPDU bounds | | ≤ 226 implicit, ≤ 330 explicit | NDSS 2024 (secondary) |
| BSM SPDU + Falcon-512 hybrid | | 301 (ECDSA) / 1,735 (Falcon) | arXiv 2608.05087 (secondary) |
| Secured CAM (field) | min 182-199 | mean 297-406 (overall 357), max 500-807 | TR 2052 Table 6-2 |
| Below-envelope headers | WSMP 5 (BSM) or GN SHB + BTP-B + LLC/SNAP 52 (CAM) | | §7 |
| MAC + FCS | 24-26 + 4 (clause UNVERIFIED) | | §4.6 |

### 9.4 Primitive descriptors

`Primitive` models (`PrimitiveDescriptor`, 03 §6). Sizes in bytes [R5 §A]:

| Primitive id | Family | pk | sk | sig or ct | Level | Source |
|---|---|---|---|---|---|---|
| `primitive/ecdsa-p256-sha256` | Signature | 33 compressed / 65 uncompressed | 32 | 64 (66 encoded in 1609.2) | 1 (128-bit) | SEC 1 §2.3.3; Ieee1609Dot2BaseTypes.asn |
| `primitive/ecdsa-brainpoolp256r1` | Signature | 33 / 65 | 32 | 64 | 128-bit | RFC 5639 §3.4, Annex A |
| `primitive/ecdsa-p384` (and brainpoolP384r1) | Signature | 49 / 97 | 48 | 96 | 192-bit | Ieee1609Dot2BaseTypes.asn; SEC 1 |
| `primitive/ed25519` (legacy stand-in only) | Signature | 32 | 32 | 64 | 128-bit | RFC 8032 §7 |
| `primitive/ecqv-p256` | ImplicitCert | reconstruction value 33 | | certificate ≈ 80 (§9.2) | 128-bit | SEC 4 §3.4-3.5 |
| `primitive/ml-dsa-44` | Signature | 1,312 | 2,560 (or 32-byte seed) | 2,420 | 2 | FIPS 204 Table 2 |
| `primitive/ml-dsa-65` | Signature | 1,952 | 4,032 | 3,309 | 3 | FIPS 204 Table 2 |
| `primitive/ml-dsa-87` | Signature | 2,592 | 4,896 | 4,627 | 5 | FIPS 204 Table 2 |
| `primitive/falcon-512` | Signature | 897 | 1,281 | 666 padded (variable ≤ 752) | 1 | PQClean `falcon-padded-512` api.h; falcon-sign.info; FIPS 206 draft status UNVERIFIED |
| `primitive/falcon-1024` | Signature | 1,793 | 2,305 | 1,280 padded (≤ 1,462) | 5 | same |
| `primitive/slh-dsa-sha2-128s` (and SHAKE) | Signature | 32 | 64 | 7,856 | 1 | FIPS 205 Table 2 |
| `primitive/slh-dsa-sha2-128f` | Signature | 32 | 64 | 17,088 | 1 | FIPS 205 Table 2 |
| `primitive/slh-dsa-*-192s/192f/256s/256f` | Signature | 48 / 48 / 64 / 64 | 96 / 96 / 128 / 128 | 16,224 / 35,664 / 29,792 / 49,856 | 3 / 3 / 5 / 5 | FIPS 205 Table 2 |
| `primitive/ml-kem-512` | Kem | ek 800 | dk 1,632 | ct 768, ss 32 | 1 | FIPS 203 Table 3 |
| `primitive/ml-kem-768` | Kem | 1,184 | 2,400 | 1,088, 32 | 3 | FIPS 203 Table 3 |
| `primitive/ml-kem-1024` | Kem | 1,568 | 3,168 | 1,568, 32 | 5 | FIPS 203 Table 3 |
| `primitive/hybrid-mldsa44-ecdsa-p256` | Signature | 1,345 raw (1,377 composite DER) | | 2,484 raw (2,492 composite maximum) | 2 | raw: FIPS 204 + SEC 1 (derived); composite: draft-ietf-lamps-pq-composite-sigs-19 Appendix A |
| `primitive/hybrid-mldsa65-ecdsa-p256` | Signature | 2,017 composite | 83 | 3,381 maximum | 3 | draft-ietf-lamps-pq-composite-sigs-19 |
| `primitive/hybrid-falcon512-ecdsa-p256` | Signature | 930 raw | | 730 raw | 1 | derived |
| `primitive/sha-256` | Hash | | | 32 | | FIPS 180-4 |

Cost anchors (`CostTable` rows keyed by `HardwareProfile`; cycles or ops/s exactly as published) [R5 §B-C; R7 §D-E]:

| Primitive | Platform | Sign | Verify | KeyGen | Source and status |
|---|---|---|---|---|---|
| ECDSA P-256 | Cortex-M4 nRF52840 at 64 MHz | 375 k cycles (5.9 ms) | 976 k cycles (15.3 ms) | 327 k (5.1 ms) | Emill/P256-Cortex-M4 |
| ECDSA P-256 | Cortex-A72 (Pi 4, OpenSSL 1.1.1d) | 4,097.4 /s | 1,550.7 /s | | gist; OS not stated |
| ECDSA P-256 | Cortex-A53 (Pi 3B / 3B+) | 1,631.1 / 1,914.7 /s | 775.3 / 908.4 /s | | gists [R7 §D8], indicative |
| ECDSA P-256 | Cortex-A76 (Pi 5, wolfSSL 5.9.1 vs mbedTLS 3.6.6) | | 14,933 vs 592 /s | | wolfSSL benchmark blog |
| ECDSA P-256 | Intel i9-11950H (wolfSSL vs mbedTLS) | 64,194 vs 4,227 /s | 61,357 vs 1,244 /s | | same |
| ECDSA P-256 | Cortex-M33 STM32H563 at 250 MHz (wolfSSL vs mbedTLS) | | 167 vs 12.1 /s | | same; wolfSSL's own NUCLEO-F446ZE rows UNVERIFIED |
| ECDSA P-256 | CAMP planning, 2 GHz processor | about 1,500 /s | about 300 /s | | CAMP EE Requirements 2016 p. 75 |
| ECDSA P-256 | Cohda MK6 (Botan, no NEON) | 7.820 ms | 0.001 ms (anomalous, as published) | | NDSS 2024 Table V |
| ECDSA P-256 | Infineon AURIX TC3xx HSM at 100 MHz | 200 /s | 100 /s | | AURIX training doc [R7 §D] |
| ECDSA P-256 verification engines | NXP SAF5400: 2,000 messages/s; Autotalks CRATON: 3 engines each < 2 ms, > 2,000 /s (Unex OBU-201U); CRATON2: > 2,500 /s (Unex OBU-301E/351U); Commsignia RS4 (SLI97 + radio engine): > 2,000; NXP SXF1800 system: > 1,000 messages/s (marketing) | | | | [R5 §C; R7 §D5, D7] |
| ECDSA P-256 signing HSMs | CRATON2 eHSM > 110 sig/s at < 9 ms; SLE97/SLI97 < 50 ms per signature (the "< 50 µs" in the Commsignia brief is UNVERIFIED, likely an OCR error) | | | | [R7 §D7, D3] |
| ECDSA P-256 network HSM | Thales Luna 7 A700 / A750 / A790: 2,000 / 10,000 / 22,000 tps | | | | Thales product brief |
| ML-DSA-44 (Dilithium2) | Skylake ref / AVX2 | 1,081,174 / 259,172 cycles median | 327,362 / 118,412 | 300,751 / 124,031 | Dilithium round-3 spec Table 1 (sizes differ slightly from FIPS 204) |
| ML-DSA-65 / -87 (Dilithium3 / 5) | Skylake ref / AVX2 | 1,713,783 / 428,587; 2,383,399 / 538,986 | 522,267 / 179,424; 871,609 / 279,936 | 544,232 / 256,403; 819,475 / 298,050 | same |
| ML-DSA-44 | Cortex-M4 pqm4 `m4f` at 24 MHz | 3,943,121 mean (1,812,557-17,009,165) | 1,421,623 | 1,426,025 | pqm4 benchmarks.csv |
| ML-DSA-65 / -87 | Cortex-M4 `m4f` | 6,193,171 / 7,947,380 | 2,415,944 / 4,193,104 | 2,516,006 / 4,275,859 | pqm4 |
| Dilithium-2 / -3 / -5 | Cortex-M7 STM32F767 at 216 MHz | 3,658 / 6,009 / 8,157 kcycles | 1,429 (6.6 ms) / 2,453 (11.4 ms) / 4,287 (19.8 ms) kcycles | 1,437 / 2,566 / 4,368 kcycles | NIST 2022 ARM paper Table I |
| ML-DSA-44 | Pi 4 / Pi 5 (liboqs) | 286.1 / 1,885.0 /s | 3,213.9 / 8,139.0 /s | 2,642.7 / 8,986.3 /s | arXiv 2503.10238 Table 8 |
| Dilithium | Cohda MK6 (Botan) | 2.634 ms | 0.189 ms (5,299 /s) | | NDSS 2024 Table V |
| Falcon-512 | Skylake AVX2 3.6 GHz | 948,132 cycles dynamic (263.37 µs); 467,964 tree | 81,036 (22.51 µs) | 26,604,000 (7,390 µs) | Pornin ePrint 2019/893 §5.2 |
| Falcon-512 | i5-8259U 2.3 GHz | 5,948.1 /s | 27,933.0 /s | 8.64 ms | falcon-sign.info |
| Falcon-512 | Cortex-M4 STM32F407 at 168 MHz (FP emulation) | 43,301,915 cycles (257.75 ms) dynamic; 21,155,551 tree | 504,051 (3.00 ms) | 171,294,112 (1,019.61 ms) | Pornin §5.3 |
| Falcon-512 / -1024 | Cortex-M7 FPU | 4,778 / 10,243 kcycles (22.1 / 47.4 ms) | 559 / 1,136 kcycles (2.6 / 5.3 ms) | 77,475 / 193,707 kcycles | NIST 2022 ARM paper Table II |
| Falcon-512 | Pi 4 / Pi 5 (liboqs) | 615.3 / 3,360.6 /s | 7,866.3 / 19,831.3 /s | 23.6 / 95.4 /s | arXiv 2503.10238 Table 8 |
| Falcon | Cohda MK6 (liboqs) | 2.152 ms | 0.446 ms (2,243 /s) | | NDSS 2024 Table V |
| Falcon-1024 | Skylake AVX2 | 1,926,252 dynamic; 942,768 tree | 160,596 (44.61 µs) | 79,164,000 | Pornin §5.2 |
| SLH-DSA-SHA2-128s / 128f (simple) | Xeon E3-1220 ref C | 2,721,595,944 / 138,610,500 | 2,712,044 / 7,757,942 | 358,061,994 / 5,590,602 | SPHINCS+ r3.1 Table 4; AVX2 Table 6: 644,740,090 / 33,651,546 sign, 861,478 / 2,150,290 verify |
| SLH-DSA-SHA2-128s / 128f | Cortex-M4 pqm4 `clean` | 7,657,558,168 / 368,575,228 | 7,471,794 / 21,923,628 | 1,007,731,522 / 15,742,990 | pqm4 |
| SPHINCS+ / XMSS | Cohda MK6 | 5.485 / 1,405.408 ms | 5.436 (184 /s) / 2.780 (359 /s) ms | | NDSS 2024 Table V |
| liboqs reference ops/s (host CPU not stated, UNVERIFIED platform) | Falcon-512 sign 176.55 /s, verify 17,246 /s; Falcon-1024 80.56 / 8,341; Dilithium2 2,099.33 / 8,752.33; Dilithium3 1,320 / 5,519; SPHINCS+-128f 23.86 / 404.73 | | | | arXiv 2503.10238 Table 6 |
| Ed25519 | Pi 4 (OpenSSL) | 2,939.1 /s | 1,327.0 /s | | arXiv 2503.10238 Table 7 |

Not found: ML-DSA-65/87, Falcon-1024 and SLH-DSA on Pi 4; Cortex-A53 liboqs; AWS CloudHSM and nShield rates; Qualcomm 9150, Infineon SLI97 alone, ATECC608 command timings; CycurHSM (all UNVERIFIED [R5, R7]). A `CostTable` row missing for a `HardwareProfile` is filled by scaling the nearest cycle count by clock ratio and tagged `scaled`, which the manifest records.

### 9.5 Certificate attachment and P2PCD

| Item | Value | Source and status |
|---|---|---|
| SAE: full certificate interval `CertAttachInt` | 450 ms | Rostami 2018 Table 1 (twice) VERIFIED (secondary) |
| SAE: "every fifth SPDU" (500 ms) industry cadence | 5 SPDUs per cycle, digest in the other 80 % | NDSS 2024 §II-A citing J2945/1 |
| SAE: attach on new neighbor | UNVERIFIED | not found |
| ETSI CAM | digest by default; certificate once, one second after the last inclusion; immediately in the next CAM on receiving a CAM from an unknown AT (timer restarted) or an `inlineP2pcdRequest` naming the own AT; `inlineP2pcdRequest` carries digests of unknown ATs and unknown AA certificates; `requestedCertificate` answers with a known CA certificate | TS 103 097 V2.1.1 §7.1.1 VERIFIED (unchanged in V2.2.1) |
| ETSI VAM | as CAM, plus "new CAM signer"; cluster VAM every ≥ 500 ms | TS 103 300-3 §6.5.3 |
| DENM | signer always certificate | TS 103 097 §7.1.2 |
| P2PCD (1609.2 clause 8) | trigger on an unknown issuer; out-of-band (HashedId3 `p2pcdLearningRequest`, separate throttled response PDUs, CA certificates only) or inline (`inlineP2pcdRequest`, response in the next SPDU's `requestedCertificate`, end-entity certificates too); mutually exclusive header fields; responders back off randomly and stop after a threshold of observed responses (three) | IEEE 1609.2a-2017 §6.3.9, §8.1-8.2.4 VERIFIED; NDSS restates 1609.2-2022 values: uniform 0-250 ms backoff, respond only if fewer than 3 responses heard |
| P2PCD configuration parameters | `p2pcd_useInteractiveForm`, `p2pcd_flavor {inline, out-of-band, none}`, `p2pcd_requestActiveTimeout`, `p2pcd_observedRequestTimeout`, `p2pcd_maxResponseBackoff` (250 ms), `p2pcd_responseActiveTimeout`, `p2pcd_currentlyUsedTriggerCertificateTime`, `p2pcd_responseCountThreshold` (3) | recommended values in security profile C.2.1.3.1; only the two bracketed values are sourced numerically, the rest `TODO: calibrate` (plan: read IEEE 1609.2-2022 Annex C.2.1.3.1) |
| Field statistics | 99.3 % of received certificates already known (Erlangen, 50 veh/km); proposal: certificate once per second plus P2PCD for pseudonym certificates: 10 × 330 = 3,300 B per 5 s → 5 × 330 + 3 × 172 = 2,166 B (−34 %) | NDSS 2024 §IV-A, §IV-B |
| Inline request rate | unknown-certificate condition in 76.6-83.9 % of in-range events; default `inlineP2pcdRequest` about 58.7 / 55.5 per s, reduced to 8.3 / 6.6 per s with trajectory-aware suppression | Yoshizawa and Preneel Table V |
| C2C-CC extras | AT change on a 32-bit hashedId8 collision; `pSecMessageFutureToleranceTime` 220 ms; `pSecCamPastToleranceTime` 2 s; `pSecMaxAcceptDistance` 10 km | RS 2037 RS_BSP_181 |
| Extended P2PCD (1609.2-2022) | CA certificate requests in any message via `contributedExtensions`; P2P distribution of large security-management messages; standalone certificates | Whyte 2022 slides (secondary); 1609.2-2025 supersedes |

The `SignerIdPolicy` in the scenario (`full_cert_every_ms: 1000 | 450`) selects the SAE or ETSI schedule; `VerificationPolicy` models (`verify-all`, `on-demand` [Krishnan and Weimerskirch 2011 via ePrint 2022/133], `prioritized`) decide which SPDUs are verified under load (receive load above 1,000 messages/s on a busy road [ePrint 2022/133 §I]; NDSS design requirement ≥ 100 verifications per 100 ms for 100 neighbors).

### 9.6 Crypto modes

`crypto-backend/modeled`: signatures are tokens {key id, message hash}; verification compares tokens; sizes and costs come from the descriptor tables; outcomes are identical to `real` by construction (I-S1, golden test on the Phase 2 scenario). `crypto-backend/real`: RustCrypto `p256` 0.14.0 (Apache-2.0 OR MIT), `pqcrypto` 0.18.1 (MIT OR Apache-2.0) over PQClean, liboqs (MIT) through `oqs-sys` when built with the `liboqs` feature, `ecqv` crate 0.1.0 (MIT OR Apache-2.0) for SEC 4 implicit certificates; SHA-256 per FIPS 180-4 [R5 §D]. Bouncy Castle ECQV support and the IEEE ASN.1 module license are UNVERIFIED. Protocol-level constants used by the cost models (developed in 05-protocols): initial batch 3,120 pseudonym certificates (20 per week × 52 × 3 years), i-period 10,080 min, lifetime 10,140 min (1 h overlap), epoch 2004-01-01 [CAMP EE Requirements 2016 §2.1.5.3.2, §2.2.7.6.1]; linkage value 9 B, seed 16 B, `LaId` 2 B [Ieee1609Dot2BaseTypes.asn; Brecht 2018 §V-B]; CRL hash entry 14 B (`HashedId10` + `Time32`), linkage entry 32 B of seeds per vehicle plus group overhead, about 40 B per entry so 10,000 entries ≈ 400 KB, daily distribution as the working assumption [Ieee1609Dot2CrlBaseTypes.asn; Brecht 2018 §VI-F; USDOT 2013].

## 10. Infrastructure connectivity and backend

### 10.1 Cellular Uu

`CellularUu` (03 §5): `coverage`, `send` (schedules delivery with a per-cell capacity and latency model; `NoCoverage` makes the caller store and forward), `handover` (returns the interruption). Tiers per 02 §7.1 (Backend row) and the constitution.

**`cellular/uu/fixed-latency`** (abstract). One-way latency per direction drawn from a fixed value or a normal distribution with the measured means; no capacity, no coverage holes unless a coverage map is given.

| Parameter | Default | Source |
|---|---|---|
| LTE first-hop RTT | 29.2 ± 4.8 ms (one way modeled as half) | Narayanan et al. WWW'20 Table 2 [R11 §A1] |
| 5G mmWave first-hop RTT | 27.4 ± 6.4 ms | same |
| total RTT to an east-coast / west-coast server | 5G 54.0 ± 4.5 / 81.9 ± 5.5 ms; 4G 58.0 ± 4.3 / 88.9 ± 5.5 ms | same |
| median 5G latency (US carriers, H2 2025) | T-Mobile 31, Verizon 32, AT&T 34 ms (FWA 50-67 ms) | Ookla via IEEE ComSoc blog [R11 §A8]; p95 UNVERIFIED |
| one-hop latency in a remote-driving budget | DSRC 2-3 ms, 5G 18-20 ms | MASA living lab [R11 §A2] |
| MOSAIC example defaults (for comparison) | uplink about 100 ms, downlink unicast about 50 ms | Eclipse MOSAIC Cell docs [R11 §A7] |

**`cellular/uu/cell-capacity-mm1`** (medium). Per-cell UL and DL capacity with an M/M/1 queue per node and Jackson's theorem for multi-node transit [Coll-Perales 2022 §VI, R11 §A3]; per-cell capacity defaults from the TR assumptions (aggregated bandwidth up to 200 MHz DL+UL below 6 GHz [TR 37.885 Table 6.1.1-1]) or the MOSAIC example caps (28 Mbit/s UL, 42.2 Mbit/s DL global) [R11 §A7]; regions may override delay, loss and capacity (MOSAIC region model). Latency components [Coll-Perales 2022 Tables IV, VI-IX, R11 §A3]: radio UL+DL 2.00 ms (low LoA, low load) or 2.60-4.55 ms (high LoA, load dependent); transport network mean / 99.99th percentile: MEC at gNB 0.402 / 0.422 ms, MEC at M1 0.835 / 0.875 ms, MEC at CN or centralized 2.355 / 2.396 ms (α = 0.1), rising to about 10.3 ms at the 99.99th percentile when undersized (α = 0.001); core network about 2.0 ms (200 km optical); internet 90th 21 ms, 99.99th 43 ms; peering remote mean 13.0 ms (90th 29.9, 99.99th 99.2) or local 0.306 ms (90th 0.431, 99.99th 1.493); V2X application server 0.0027-0.0031 ms mean at MEC or 0.035-3.295 ms centralized for 2,080-41,600 packets/s; 152 processors needed centralized at 41,600 packets/s versus 1 at MEC. Requirement anchors: low LoA 25 ms at 90 %, high LoA 10 ms at 99.99 % [Coll-Perales Table III]; TR 38.913: control plane 10 ms, URLLC user plane 0.5 ms UL and DL, eMBB 4 ms, eV2X 300 B at 1 − 1e−5 in 3-10 ms [R11 §A4]. Delay distribution option: MOSAIC `GammaRandomDelay` (α = 2, β = 2 parameterized by the expected delay) and `GammaSpeedDelay` [R11 §A7].

**`cellular/uu/handover-outage`** (high). Adds handover interruption and outages: LTE intra- or inter-frequency handover with a known target `T_interrupt = T_IU + 20 ms`; unknown target adds `T_search` 80 ms; RRC procedure delay +50 ms; E-UTRA to UTRA `T_IU + T_sync + 50 + 10·F_max` ms (known) or `+150` (unknown) [TS 36.133 §5.1.2.1.2, §5.3.1.1.2, R11 §A5]; packet-loss percentiles 50th / 75th / 99th = 0.01 / 0.1 / 1.2 % (5G mmWave stationary LOS) [R11 §A1]; 31 primitive handoffs and 13 4G-5G bounces in an 8-minute urban walk, throughput 0-954 Mbit/s, 4G-to-5G downgrade after about 10 s of inactivity [R11 §A1]; coverage PDR near 100 % with localized drops to about 90 % in dense canyons and intersections [MASA, R11 §A2]. ns-3 5G-LENA notes (for the cross-validation harness): X2 handover is seamless not lossless, no handover-failure recovery, UE-side RLF per TS 36.331/36.133 T310, gNB-side RLF not implemented [R11 §A5]. Cell layout: ISD 500 m urban macro, 1,732 m highway (500 optional), macro Tx 49 dBm below 6 GHz, BS noise figure 5 dB [TR 37.885, TR 38.913, R11 §A4]; Uu path loss from §3.3.

Ignores: `abstract` ignores capacity, coverage geometry and handover; `medium` ignores handover interruption, RLF, and per-packet loss correlation; `high` ignores scheduler details, HARQ on Uu, beam management (no LENA-level PHY; ns-3 5G-LENA remains the external cross-check, 02 ADR 0006).

### 10.2 Backhaul

`backhaul/fixed` (abstract): per-link one-way latency and capacity; defaults per medium: fiber and microwave from the transport-network figures above (0.4-2.4 ms mean), cellular backhaul from §10.1 (an RSU on cellular backhaul is a UE). Deployment facts for scenario presets [R11 §A6]: Tampa THEA started with 45 of 47 RSUs on cellular ($35 per month per RSU at 5 GB rising to $100 at 20 GB, about $4,500 per month fleet) and moved the express-lane RSUs to fiber; Wyoming I-80 mixes fiber, microwave and wireless plus satellite for traveler information; NYC 470 RSUs and 3,000 vehicles (backhaul medium UNVERIFIED). `backhaul/measured-tn` (medium and high): the Coll-Perales M/M/1 transport-network chain with the α-dimensioned capacity so that the 99.99th percentile blows up under undersizing, as measured.

### 10.3 Backend service models

`ServiceModel` (03 §8): `service-model/fixed-latency` (abstract: constant service time per operation class), `service-model/mmc-batching` (medium: M/M/c per entity with `c` servers and a batch policy: window `T_batch` and maximum batch size; service time per batch = setup + per-item cost from the primitive tables §9.4), `service-model/measured-availability` (high: per-request service-time distributions, availability with MTBF/MTTR, retries). Anchors: application-server latency 0.0027-0.0031 ms at MEC and 0.035-3.295 ms centralized [Coll-Perales Table IX]; Thales Luna 7 signing 2,000 / 10,000 / 22,000 ECC P-256 tps (A700 / A750 / A790), RSA-2048 1,000 / 5,000 / 10,000 tps [R5 §B.5]; CAMP software baseline about 1,500 sign/s and 300 verify/s on a 2 GHz processor [CAMP EE Req. 2016]; batch of 3,120 certificates per request [CAMP EE Req. §2.2.7.6.1]; CRL daily [USDOT 2013; Brecht 2018 §VI-G]. Availability and MTBF values: `TODO: calibrate` (plan: use the outage events of the scenario timeline as the only source until an SCMS operator publishes availability figures). Ignores: `abstract` ignores queueing; `medium` ignores failures and retries; `high` ignores geographic redundancy and load balancing between replicas (modeled as one M/M/c with `c` = replica count).

## 11. Safety applications and surrogate safety measures

`SafetyApp` models consume the neighbor table and the own position estimate (03 §6) and emit `Warning` records; surrogate safety measures are `MetricProvider`s over ground truth (08 §2.5). Definitions from the CAMP VSC-A final report [DOT HS 811 492A, R11 §B1]:

| Model id | Definition | Numeric triggers | Status |
|---|---|---|---|
| `safety-app/eebl-vsca` | host vehicle broadcasts a self-generated emergency-brake event; remote vehicles judge relevance and warn | BSM Part II event flag at deceleration ≥ 0.4 g; ABS, stability and traction flags require ≥ 100 ms activation | VERIFIED |
| `safety-app/fcw-vsca` | warns of an impending rear-end collision with a remote vehicle ahead in the same lane and direction | time-to-collision threshold `TODO: calibrate` (plan: SAE J2945/1 or the VSC-A companion volume; not in the accessible report) | threshold UNVERIFIED |
| `safety-app/bsw-lcw-vsca` | warns during a lane-change attempt if the blind-spot zone is or will be occupied; advisory otherwise | zone geometry `TODO: calibrate` (same plan) | |
| `safety-app/dnpw-vsca` | warns during a passing maneuver if the passing zone is occupied by an oncoming vehicle | passing-zone length `TODO: calibrate` | |
| `safety-app/ima-vsca` | warns when entering an intersection is unsafe; initial scope stop-sign-controlled and uncontrolled intersections | time-to-intersection threshold `TODO: calibrate` (not in the accessible report) | threshold UNVERIFIED |
| `safety-app/clw-vsca` | host broadcasts a self-generated control-loss event | as EEBL flags | |
| `safety-app/vru-warning` | warns of a VRU (from PSM or VAM, or perception) on a collision course | reuse the FCW TTC once calibrated | |

Track-test speeds used by VSC-A for true-positive scenarios: 15-50 mph by scenario (for example FCW-T5 40 mph, IMA-T3 15 / 25 / 35 / 45 mph) [R11 §B1]; radar re-acquisition after a lead-vehicle cut-out about 5 s versus continuous tracking with DSRC [R11 §B1]; reference forward-looking radar 76 GHz, 3-150 m for 10 m² RCS, range rate −64 to +33 m/s, azimuth FOV ±7.5°, 10 Hz [VSC-A Table 3]. The J2945/1 tracking-error threshold about 0.5 m seen in a search summary is UNVERIFIED (the Rostami `vTEMax` 0.5 m of §6.4 is the sourced value).

`metric/ssam/ttc-pet-drac` [FHWA-HRT-08-051, R11 §B2]: TTC = minimum time to collision during a conflict from current positions, speeds and future trajectories, default threshold 1.5 s (VERIFIED, "as suggested in previous research"); PET = time between the first vehicle leaving a position and the second arriving at it, zero meaning a collision, default threshold UNVERIFIED (commonly < 5 s, not confirmed; `TODO: calibrate`, plan: read FHWA-HRT-08-050 §3); DRAC or DR = initial deceleration rate of the second vehicle (first negative acceleration during the conflict), threshold UNVERIFIED (same plan). Ghost-vehicle to false-warning chain: pseudonym change leaves stale LDM entries (one ego sees 3 + 2 entries for 2 real vehicles) and the silent period makes the reappearance look anomalous [TR 103 415 V2.1.1 §4.4.2]; CPM lets one certificate back many perceived objects, easing ghost fleets [TR 103 460 Annex C.3]; a quantified ghost-to-false-FCW experiment was not found (UNVERIFIED), so the engine measures it (07-threats §5) rather than assuming a rate. Plausibility gate for EEBL reports: the preceding vehicle's deceleration must be positive within 500 ms before the claimed `detectionTime` [TS 103 759 V2.2.1, R11 §E2].

## 12. Perception and jamming

### 12.1 Sensors

`Perception` (03 §8): `perception/disc-sensor` (medium: range, FOV, detection probability by range, no occlusion) and `perception/occluded-sensor` (high: occlusion by buildings and vehicles through `ObstacleModel::los`, per-sensor error models). Datasheet anchors [R11 §C1]:

| Sensor | Range | FOV | Update | Status |
|---|---|---|---|---|
| Continental / Aumovio ARS 408-21 (77 GHz FMCW) | 70 m short-range mode, 250 m far-range mode (1,200 m extended variant, high-RCS, clear FOV) | not on the cached page | 17 scans/s; > 120 clusters | VERIFIED (cached product page) |
| ARS540 (4D imaging radar) | about 300 m | ±60° or 120° (conflicting secondary sources) | | range secondary; FOV UNVERIFIED |
| SRR520 (77 GHz short range) | 100 m at 0° | detection ±90°, measurement ±75° | 50 ms; speed accuracy ±0.07 km/h | reseller listings only, indicative |
| SRR320 | UNVERIFIED (no datasheet) | | | |
| MFC 500 (mono camera) | UNVERIFIED | about 125° horizontal | up to 8 MP | secondary (social media and press snippet) |
| HFL110 (flash LiDAR) | 50 m | 120° × 30° | 25 fps; 128 × 32 depth frame; 1064 nm Class 1 | secondary (press coverage) |
| VSC-A reference FLR | 3-150 m (10 m² RCS) | ±7.5° | 10 Hz | VERIFIED [VSC-A Table 3] |

Detection probability versus range: no manufacturer curve for any sensor (UNVERIFIED) [R11 §C1]; `perception/disc-sensor` uses a step function (1 inside range and FOV) by default with the shape `TODO: calibrate` (plan: fit a logistic in range on a public detection dataset such as nuScenes radar annotations, recorded per sensor class). Default sensor set for an equipped vehicle: one ARS 408-class front radar (250 m far range, FOV `TODO: calibrate`) and one VSC-A FLR-class fallback (150 m, ±7.5°).

### 12.2 CPM object quality

`perception/cpm-quality-ts103324` computes the fields the CPM generator needs [TS 103 324 V2.1.1 §3, §7.1.7-7.1.8, R11 §C2]: classification confidence is a probability per class whose sum may not exceed 100 %; confidence values are the estimated absolute accuracy at a 95 % level; perception-region confidence quantifies the likelihood that objects or free space are correctly detected in a region; a perceived object carries a mandatory position and optional velocity, acceleration, angles, angular velocity and up to four covariance components; `objectAge` is the time the object has been known. `objectPerceptionQuality`: inputs `oa` (ms), sensor confidence `c_t` in [0, 1], detection success `d_t` in {0, 1}; `EMA_t = α·c_t + (1 − α)·EMA_(t−1)`, `r_c = floor(EMA_t·15)`, likewise `r_d` from `d_t`, `r_oa = min(floor(oa/100), 15)`, `quality = floor((w_d·r_d + w_c·r_c + w_oa·r_oa)/(w_d + w_c + w_oa))` [TS 103 324 §7.1.8.6]; the weights and α are `TODO: calibrate` (plan: the TS cites [i.4] for them; read that reference). `sensorType` special values `localAggregation` (12) and `itssAggregation` (13); VRU clusters via `groupSubClass` from VAM.

### 12.3 Jamming

Jammers are `Attacker` models with `TransmitRaw` actions and enter the interference sums of §4-5 as transmitters (07-threats §2.2). Profiles and anchors [Puñal, Aguiar and Gross 2012, R11 §D1]:

| Model id | Behavior | Anchors |
|---|---|---|
| `attacker/jammer/constant` | continuous OFDM-like noise in the channel | WARP jammer 16.75 dBm measured at 5.9 GHz (spec about 18 dBm at 2.4 GHz); legitimate Linkbird Tx 17.58 dBm (spec 21 dBm); blind area about 250 m in open space (platoon) and 167 m at a dense urban crossroad at 30 km/h with the jammer 33 m from the junction indoors; PDR degradation tracks SINR |
| `attacker/jammer/reactive` | transmits only when energy above a trigger threshold is sensed | trigger −75 dBm RSSI; blind area about 170 m in open space; PDR down to 60 % even at high SINR in dense urban with the Tx near the jammer; low impact with reduced line of sight; produces PDR = 0 dropouts uncorrelated with SINR dips (a detection cue) |
| `attacker/jammer/constant-pilot` | jams the OFDM pilot subcarriers only | total power 2.42 dBm |

Receiver-side model constants from the same study: noise floor −86 dBm; RSSI-to-SINR map `γ[dB] = 0.8565·σ − 86.35` (least squares). ns-3 and Veins jammer module parameters are UNVERIFIED (not located). Jamming range is bounded by the attacker's own propagation (a survey states it raises latency and reduces reliability only within range [arXiv 2003.07191, R11 §D2]). GNSS jamming and spoofing use §3.8 (spoofing 91-547 m effective range with an SDR).

## 13. Validation plan

The nightly suite (`tests/validation/`) runs each family's reference scenarios against the literature curves below and writes a report per model card (`validation.status` moves to `literature-checked` only when every target passes). Automated check design: each row is a `ValidationCase { scenario, tiers, seeds (default 10), statistic, target, tolerance, source }`; the statistic is computed from the recorded MCAP channels (never from a side channel, 02 §3); tolerances are the stated ones or, for digitized figures, the digitization uncertainty declared in the sheet (±0.3 dB for Sjöberg; unstated for the PDR curves, so ±5 percentage points is used and recorded as a design choice); a failure lists the bins that missed, the model ids and parameter-set ids involved, and blocks the tier from being labeled `literature-checked`. Propagation rows come from R3 §H; PHY/MAC rows from R1, R2 and R2d; the rest from R10, R5, R7.

| Family | Case | Target | Tolerance | Source |
|---|---|---|---|---|
| Propagation (`high`) | PDR versus distance, car following, 100 and 500 B beacons, 3 and 6 Mbit/s, 100 ms | PDR ≈ 0.95-1.0 at about 25 m (clear LOS); ≈ 0.5-0.6 at about 150 m with intermittent LOS obstruction; → 0 with a truck between Tx and Rx | within the stated bands | Martelli, Renda and Santi (VTC 2011) [R3 §H.1] |
| Propagation (`high`) | PDR cliff by link class | PDR ≈ 0 beyond 1,000 m LOS highway, 500 m LOS urban, 400 m NLOSv, 300 m NLOSb | ±1 bin (25 m) | Boban thesis Table 4.5 [R3 §D.3] |
| Propagation (`medium`, `high`) | vehicle obstruction | > 20 dB for one obstructing vehicle; 27 dB truck at 26 m; 12 dB van at 20 m; LOS to OLOS offset 8.6-10 dB | ±3 dB | Boban 2011, Meireles 2010, Abbas 2015 [R3 §A.5, §D.1] |
| Propagation (`high`) | RSS versus distance through buildings | Sommer β 9 dB per wall, γ 0.4 dB/m reproduce the WONS 2011 Figs. 6-9 shape | ±3 dB on the mean | Sommer 2011 [R3 §C.1, §H.1] |
| Propagation | Bai and Krishnan 2006 PDR curve | UNVERIFIED (full text not retrieved); placeholder case disabled | | [R3 §H.1] |
| PHY 802.11p (`medium`, `high`) | SNR at 10 % PER, AWGN, 6 Mbit/s | model 6.6 dB at 400 B; measured 11.5-11.7 dB at 300 B with `rx_impl_loss_db` = 5 | ±0.3 dB (model), ±0.5 dB (measured) | Pei and Henderson 2010; Sjöberg et al. [R1 §C] |
| PHY 802.11p (`high`) | SINR at 10 % PER under fading | bracket −0.24 (MCS0 190 B highway LOS), 3.10 (highway NLOS), 3.86 (MCS2 350 B urban LOS), 5.92 (crossing NLOS), 6.78 dB (highway NLOS) | ±1 dB | WiLabV2Xsim tables [R2 §C] |
| MAC 802.11p (`high`) | capture | 5 dB threshold at 3 Mbit/s reproduces the Torrent-Moreno capture behavior | qualitative | [R1 §B.4] |
| DCC (`all`) | reactive state transitions versus CBR | states change at 30 / 40 / 50 / 60 % (Table A.1) or 30 / 40 / 50 / 65 % (Table A.2) with the listed rates and `T_off` | exact crossing ±1 `T_CBR` | TS 102 687 Annex A [R1 §D.2, R3 §H.2] |
| DCC (`all`) | adaptive convergence with rising density | steady-state CBR tracks 0.68, neither saturating near 1.0 (uncontrolled EDCA) nor dropping well below 0.55 | ±0.05 | TS 102 687 Table 3 [R3 §H.2] |
| DCC (`all`) | EN 302 571 floor | `T_off ≥ 25 ms`; for CBR ≥ 0.62 `T_off ≥ min{1000, T_on·4000·(CBR − 0.62)/(CBR − 1)}`; examples 351.9 ms at CBR 0.68 and 692.3 ms at 0.75 for `T_on` 1 ms | exact | EN 302 571 §4.2.10.2; TS 103 175 Table 2 [R1 §D.3-D.4] |
| DCC (`all`) | CBR versus density curve | UNVERIFIED (no numeric table found; secondary figure inaccessible); case emits the curve for later comparison | | [R3 §H.2] |
| CAM generator | field interval statistics | mean CAM interval 0.33-0.47 s on drives; 63-76 % of CAMs with certificate; size mean 297-406 B | within bands | C2C-CC TR 2052 [R4 §A.2, §B] |
| J2945/1 rate control | ITT versus density | `MaxITT` 100 ms up to 25 vehicles in 100 m, 600 ms cap; power 20 → 10 dBm between 50 % and 80 % CBP | exact per the equations | Rostami 2018 [R1 §E] |
| C-V2X Mode 4 (`high`) | PDR versus distance, Highway Slow 10 pps | 0.97 / 0.95 / 0.90 / 0.79 / 0.66 / 0.45 at 0 / 100 / 200 / 300 / 400 / 500 m; 50 pps 0.91 / 0.58 / 0.27 / 0.11 at 25 / 100 / 200 / 300 m | ±5 pp | Molina-Masegosa 2017 Fig. 3 [R2d] |
| C-V2X Mode 4 (`high`) | occupancy and collisions | 32.46 % / 3.38 % (10 pps slow), 80.91 % / 56.64 % (50 pps slow), 17.08 % / 0.78 % and 62.08 % / 23.33 % (fast) | ±2 pp | Molina-Masegosa 2017 Table 2 [R2d] |
| C-V2X Mode 4 (`high`) | PRR within 100 m urban / 200 m highway | MCS 4: 0.63 / 0.58 / 0.66; MCS 7: 0.56 / 0.61 / 0.74; MCS 14: 0.50 / 0.60 / 0.70 (Cologne / Bologna / highway) | ±5 pp | Bazzi 2018 Fig. 3 [R2d] |
| C-V2X Mode 4 (`high`) | 3GPP calibration | freeway about 80 % PRR at 320 m; urban 15 km/h 90 % at 50 m; urban 60 km/h about 60 % at 150 m | ±5 pp | TR 36.885 §9.1.1 Table 9.1-1 [R2 §E] |
| C-V2X Mode 2 (`high`) | PRR versus distance, MCS 21, 350 B | worst 0.98 / 0.89 / 0.74 / 0.62 / 0.30 / 0.04 at 10 / 60 / 90 / 100 / 120 / 150 m (SCS 15 with IBE); best 1.0 / 0.85 / 0.76 / 0.46 / 0.09 at 10 / 90 / 100 / 120 / 150 m (SCS 60 without IBE) | ±5 pp | Todisco 2021 Fig. 7 [R2d] |
| C-V2X Mode 2 (`high`) | range at PRR 0.9, MCS 4, SCS 15 | about 190 / 155 / 100 m at 50 / 100 / 200 veh/km; 110 m without and 170 m with the 20 % L2 list | ±10 m | Todisco 2021 Figs. 8, 10(a) [R2d] |
| C-V2X BLER | LUT reproduction | 10 % BLER at about 8.7 dB (QPSK r0.7) and 5.9 dB (r0.5), 190 B; hard thresholds 2.76 dB (MCS 4) and 7.30 dB (MCS 7), 300 B | ±0.5 dB | R1-160284 via Gonzalez-Martin 2019; Bazzi 2018 [R2e] |
| C-V2X loss decomposition | `LossCause` shares sum to `1 − PDR` as `P_HD + P_SEN + P_PRO + P_COL` | structural | exact | Gonzalez-Martin 2019 [R2 §E] |
| Abstract radio (`abstract` versus `high`) | §4.9 procedure per RAT | ≤ 5 pp PDR per 25 m bin at each calibration density; CBR estimate ±0.05 | as stated | 02 §7.3, I-R4 |
| Mobility (`medium`) | fundamental diagram | capacity drop 5-20 % (IDM+ACC 5-15 %); jam density 140 veh/km; wave speed about −15 km/h; `Q_cv` about 1,050 veh/h; uncongested regime 300-2,200 pcphpl | within bands | Treiber 2000; Kesting 2010; FHWA Traffic Flow Theory [R10 §B15] |
| Mobility (`medium`) | lane-change rate | 450-1,400 changes/h/km at 10-15 veh/km/lane depending on politeness | within band | Kesting 2007 [R10 §B3] |
| Mobility (`medium`) | roundabout capacities | 1,800 veh/h circulating and 1,200 veh/h exit ceilings; degree of saturation ≤ 0.85 | ±10 % | FHWA Roundabouts Guide [R10 §B17] |
| Mobility (`high`) | SUMO parity | native Krauss port matches SUMO on a golden trajectory | bit-identical after quantization | ADR 0005 |
| Node compute | verification throughput versus datasheets | with the `dsrc-hw-verify` profile: about 2,000 verifications/s (SAF5400), > 2,000 (CRATON), > 2,500 (CRATON2); software profiles: 1,550.7 /s (Cortex-A72), 775.3-908.4 /s (Cortex-A53), 14,933 /s (Cortex-A76 wolfSSL), 100 /s (AURIX HSM), about 300 /s (CAMP planning); Cohda MK6: Falcon 2,243 /s, Dilithium 5,299 /s, SPHINCS+ 184 /s, XMSS 359 /s | ±10 % of the anchor | [R5 §B-C; R7 §D-E] |
| Node compute | signing latency and rate | CRATON2 eHSM > 110 sig/s at < 9 ms; SLE97/SLI97 < 50 ms; AURIX 200 sig/s; ECDSA on MK6 7.820 ms | ±10 % | [R7 §D3, D7] |
| Node compute | PQ frame-duration versus verify-time capacity | v_max 165 (ECDSA), 101 (PH-Falcon), 53 (PH-Dilithium), 21 (PH-SPHINCS+), 49 (PH-XMSS) frame-limited; 67,521 / 224 / 529 / 18 / 35 verify-limited | ±10 % | NDSS 2024 [R7 §E3] |
| GNSS | stationary error quantiles | horizontal 3.07 / 5.30 / 9.38 m at 68 / 95 / 99 % (open sky); urban canyon mean 31.02 m; outage 95th percentile < 7 s | ±10 % | Reid 2019; Wen and Hsu [R3 §G] |
| Message sizes | size model versus anchors | §8.4 anchor spreads; real codec equality when the module is present | exact or spread | [R4 §B-C] |
| Security envelope | encoded overhead | digest ≈ 93-94 B, certificate ≈ 87 + cert; BSM SPDU 180 / 250; CAM field mean 357 B | exact for the real encoder; spread for the size model | [R4 §C-D] |
| Cellular Uu | latency distributions | first-hop RTT 29.2 ± 4.8 ms (LTE), 27.4 ± 6.4 ms (5G); TN 99.99th percentiles 0.422 / 0.875 / 2.396 ms by placement | ±10 % | Narayanan 2020; Coll-Perales 2022 [R11 §A] |
| Jamming | blind areas | constant about 250 m open space, 167 m urban; reactive about 170 m, PDR down to 60 % | ±25 m | Puñal 2012 [R11 §D1] |
| Detection | F2MD and TS 103 759 thresholds reproduce their check semantics on the legacy attack catalog | detector fires on the attack it targets and not on the benign reference run above the legacy false-positive rate | per detector | §14 |

Cases whose targets are UNVERIFIED (Bai and Krishnan, the CBR-density curve, Bazzi 2019 PDR figures, Molina-Masegosa's per-distance plotted values beyond the digitized points) are present as disabled rows so the report shows what is missing.

## 14. Detector thresholds (reference defaults)

Three detector families ship with their thresholds as cited defaults; all run on node belief only (I-T2).

**`detector/f2md-checks`** [veins-f2md source, cloned and read: `F2MDParameters.h`, `MdChecksTypes.h`, `ExperiChecks.cc`, R10 §C4]. Nineteen checks: ProximityPlausibility, RangePlausibility, PositionPlausibility, SpeedPlausibility, PositionConsistancy, PositionSpeedConsistancy, PositionSpeedMaxConsistancy, SpeedConsistancy, BeaconFrequency, Intersection, SuddenAppearence, PositionHeadingConsistancy, kalmanPSCP, kalmanPSCS, kalmanPSCSP, kalmanPSCSS, kalmanPCC, kalmanPACS, kalmanSCC; classes Genuine, LocalAttacker, GlobalAttacker.

| Constant | Value | Used by |
|---|---|---|
| MAX_PROXIMITY_RANGE_L, _W, MAX_PROXIMITY_DISTANCE | 30 m, 3 m, 2 m | ProximityPlausibility (longitudinal box, lateral box, distance) |
| MAX_CONFIDENCE_RANGE | 10 | general confidence bound |
| MAX_PLAUSIBLE_RANGE | 420 m | RangePlausibility |
| MAX_TIME_DELTA | 3.1 s | PositionSpeed(Max)Consistancy window |
| MAX_DELTA_INTER | 2.0 s | Intersection window |
| MAX_SA_RANGE, MAX_SA_TIME | 420 m, 2.1 s | SuddenAppearence |
| MAX_KALMAN_TIME | 3.1 s | all Kalman checks |
| KALMAN_POS_RANGE, KALMAN_SPEED_RANGE, KALMAN_MIN_POS_RANGE, KALMAN_MIN_SPEED_RANGE | 1.0, 4.0, 4.0, 1.0 | Kalman confidences and floors |
| MIN_MAX_SPEED, MIN_MAX_ACCEL, MIN_MAX_DECEL | 40 m/s, 3 m/s², 4.5 m/s² | fallback plausible maxima (`MAX_PLAUSIBLE_*`) |
| MAX_MGT_RNG, MAX_MGT_RNG_DOWN, MAX_MGT_RNG_UP | 4, 6.2, 2.1 | consistency margins |
| MAX_BEACON_FREQUENCY | 0.9 s | BeaconFrequency minimum inter-beacon time |
| MAX_DISTANCE_FROM_ROUTE, MAX_NON_ROUTE_SPEED | 2 m, −1 | PositionPlausibility (map matching) |
| MAX_HEADING_CHANGE | 90° | PositionHeadingConsistancy |
| DELTA_BSM_TIME, DELTA_REPORT_TIME | 5 s, 5 s | storage windows |
| POS_HEADING_TIME | 1.1 s | position and heading time gate |
| MAX_TARGET_TIME, MAX_ACCUSED_TIME | 2 s, 2 s | retention |

F2MD attack and pseudonym parameters kept for the attacker and protocol presets: `LOCAL_ATTACKER_PROB` 0.05; magnitudes `RandomPosOffsetX/Y` 70, `RandomSpeedX/Y` 40, `RandomSpeedOffsetX/Y` 7, `RandomAccelX/Y` 2, `StopProb` 0.05, `StaleMessages_Buffer` 60, `DosMultipleFreq` 4, `ReplaySeqNum` 6, `SybilVehNumber` 5, `SybilDistanceX/Y` 5 / 2; pseudonym change `Period_Change_Time` 240 s, `Tolerance_Buffer` 10, `Period_Change_Distance` 80 m, `Random_Change_Chance` 0.1 [R10 §C4]. The F2MD paper's detection and false-positive rates and MA thresholds are UNVERIFIED (HAL PDF blocked) [R11 §E3].

**`detector/ts103759-observations`** [TS 103 759 V2.2.1 (2026-01), R11 §E2; R10 §C5]: EEBL plausibility window 500 ms before the claimed `detectionTime`; detection-time consistency against 80 % of the minimum threshold for slow-down, dangerous-situation and EEBL events; one detection-time bound example ≤ 180 ms; speed-exceedance example > 80 km/h (`upstreamTraffic`); same-event correlation distance 1 km for `dangerousEndOfQueue` and `trafficCondition`, 100 m otherwise; DENM valid only if event trust `ETR(E, j) ≥ TrustThreshold` (value `TODO: calibrate`, plan: the TS leaves it predefined by the deployment; take the C2C-CC profile value when published). Observation classes 1-5 (implausible values; inconsistency with previous messages, with the local environment and LDM, with on-board sensors, with other stations) are the detector interface in 07-threats §3.1; TR 103 460 supplies the taxonomy (ART, eART, CoE, MPP, SAW, LEAVE, P2DAP) [R10 §C6].

**`detector/legacy-12`** (`code (legacy)` [`run.py` L316-364, L2015-2037, L2811-2870]): `consistency_threshold_m` 5.0; `heading_threshold_deg` 35; `detector_lag_s` 1.5 (reference fix age); `detector_z_threshold` 3.0 (residual in broadcast-uncertainty sigmas); `detector_min_consec` 2; `sybil_min_certs` and `sybil_cell_m` (module constants `_SYBIL_MIN`, `_CELL_M`, co-location cell with heading in 45° sectors); `art_max_m` 150 (tolerance beyond the receiver's range); `offroad_tol_m` 15; `max_accel_mps2` 12; `freq_max` 6 beacons per interval; `stale_max_s` 5; `vru_max_plausible_speed_mps` 10; `denm_implausible_speed_mps` 6 with the brake-specific bound 4.5 (= benign 4.0 + 0.5); certificate validity window ±1 s. Formulas: `positionSpeedInconsistency = max(0, |disp − v_avg·dt| − 0.3·|Δv|·dt)/(Z·tol)` with `tol = max(conf, 0.5·consistency_threshold_m)`; `positionJump = disp/(v_avg·dt + Z·tol + consistency_threshold_m)`; `implausibleAcceleration = (|Δv|/dt)/max_accel`; `headingInconsistency = angdiff(claimed, bearing)/heading_threshold_deg` on a one-step baseline; `acceptanceRangeThreshold = max(0, d_rx − rr)/art_max_m`; `beaconFrequency = count/freq_max`; `staleOrReplay = age/stale_max_s`; `mapOffRoad = dist_to_road/offroad_tol_m`; `kalmanConsistency = |residual|/(2·consistency_threshold_m + conf)` with the alpha-beta gains 0.5 and 0.3; `sybilCoLocation = certs_in_cell/sybil_min_certs`; `signatureVerification` and `certValidity` fire at 1.5; frozen position sets `constantPositionFrozen` 1.5 and `staleOrReplay` 1.2. All `detnorm ≈ 1 at the firing threshold` (the ML contract, 01 §3.3). MA operating point kept as `legacy-window`: k = 3 reporters, 4 distinct seconds, 3 s span, 15 s window, report budget 30, reputation 40, report probability 0.9.

## References

Standards and regulations
- ETSI EN 302 663 V1.3.1 (2020-01), ITS-G5 access layer: https://www.etsi.org/deliver/etsi_en/302600_302699/302663/01.03.01_60/en_302663v010301p.pdf
- ETSI EN 302 571 V2.1.1 (2017-02), ITS radio equipment in 5,855-5,925 MHz: https://www.etsi.org/deliver/etsi_en/302500_302599/302571/02.01.01_60/en_302571v020101p.pdf
- ETSI TS 102 687 V1.2.1 (2018-04), DCC: https://www.etsi.org/deliver/etsi_ts/102600_102699/102687/01.02.01_60/ts_102687v010201p.pdf
- ETSI TS 103 175 V1.1.1 (2015-06), cross-layer DCC; ETSI TR 101 612 V1.1.1 (2014-09), cross-layer DCC report
- ETSI TS 102 724 V1.1.1 (2012-10), harmonized channel specifications
- ETSI EN 302 637-2 V1.4.1 (2019-04), CAM: https://www.etsi.org/deliver/etsi_en/302600_302699/30263702/01.04.01_60/en_30263702v010401p.pdf
- ETSI EN 302 637-3 V1.3.1 (2019-04), DENM: https://www.etsi.org/deliver/etsi_en/302600_302699/30263703/01.03.01_60/en_30263703v010301p.pdf
- ETSI TS 103 300-3 V2.2.1 (2023-02), VAM: https://www.etsi.org/deliver/etsi_ts/103300_103399/10330003/02.02.01_60/ts_10330003v020201p.pdf
- ETSI TS 103 324 V2.1.1 (2023-06), CPM: https://www.etsi.org/deliver/etsi_ts/103300_103399/103324/02.01.01_60/ts_103324v020101p.pdf
- ETSI TR 103 562 V2.1.1 (2019-12), CPS analysis: https://www.etsi.org/deliver/etsi_tr/103500_103599/103562/02.01.01_60/tr_103562v020101p.pdf
- ETSI TS 103 301 V2.1.1 (2021-03), infrastructure services: https://www.etsi.org/deliver/etsi_ts/103300_103399/103301/02.01.01_60/ts_103301v020101p.pdf
- ETSI TS 103 097 V2.1.1 (2021-10): https://www.etsi.org/deliver/etsi_ts/103000_103099/103097/02.01.01_60/ts_103097v020101p.pdf ; V2.2.1 (2026-03): https://www.etsi.org/deliver/etsi_ts/103000_103099/103097/02.02.01_60/ts_103097v020201p.pdf
- ETSI EN 302 636-4-1 V1.4.1 (2020-01), GeoNetworking: https://www.etsi.org/deliver/etsi_en/302600_302699/3026360401/01.04.01_60/en_3026360401v010401p.pdf
- ETSI EN 302 636-5-1 V2.2.1 (2019-05), BTP: https://www.etsi.org/deliver/etsi_en/302600_302699/3026360501/02.02.01_60/en_3026360501v020201p.pdf
- ETSI TS 103 759 V2.2.1 (2026-01), misbehavior reporting; ETSI TR 103 460 V2.1.1 (2020-10): https://www.etsi.org/deliver/etsi_tr/103400_103499/103460/02.01.01_60/tr_103460v020101p.pdf ; ETSI TR 103 415 V2.1.1 (2025-03); ETSI TR 103 257-1 V1.1.1 (2019-05)
- IEEE Std 1609.2a-2017 (ETSI docbox copy): https://docbox.etsi.org/STF/Archive/STF538_TC_ITS/STFworkarea/libaries/IEEE_Std_1609_2a-2017.pdf ; IEEE 1609.2 ASN.1 (ETSI forge): https://forge.etsi.org/rep/ITS/asn1/ieee1609.2 ; IEEE 1609.2-2022: https://standards.ieee.org/ieee/1609.2/10258/ ; IEEE 1609.3-2020: https://standards.ieee.org/standard/1609_3-2020.html ; 1609.3 ASN.1 (TCI mirror): https://github.com/certificationoperatingcouncil/TCI_ASN1/tree/master/TCI%20Interface/ASN1/1609dot3 ; IEEE PSID tutorial: https://standards.ieee.org/content/dam/ieee-standards/standards/web/documents/tutorials/psid.pdf
- 3GPP TR 36.885 V14.0.0 (2016-06); 3GPP TR 37.885 V15.3.0 (2019-06); 3GPP TR 38.913 V14.2.0 (ETSI TR 138 913): https://www.etsi.org/deliver/etsi_tr/138900_138999/138913/14.02.00_60/tr_138913v140200p.pdf ; 3GPP TS 36.212/36.213/36.214 V14.4.0 and TS 36.331 V14.4.0; TS 36.214 V16.1.0 (ARIB mirror); 3GPP TS 38.211/38.212 v16.6.0/38.213/38.214 v16.4.0/38.215 v16.3.0/38.321/38.331 (Release 16); ETSI TS 136 133 (LTE RRM): https://www.etsi.org/deliver/etsi_ts/136100_136199/136133/
- FCC First Report and Order, ET Docket 19-138 (2020, fact sheet 2020-10-28; cited as FCC 20-51 in R1 and FCC 20-164 in R2/R2d); FCC 24-123 Second Report and Order (adopted 2024-11-20); FCC DA 23-343 (2023-04-24)
- SAE J2945/1_202004: https://standards.globalspec.com/std/14220512/sae-j2945-1 ; J2945/1B_202212: https://www.sae.org/standards/content/j2945/1b_202212/ ; J2945/9_201703: https://www.sae.org/standards/content/j2945/9_201703 ; J2735ASN_202007: https://www.sae.org/standards/content/j2735asn_202007/
- CTI 4501 v01.01 Connected Intersections Implementation Guide: https://www.ite.org/ITEORG/assets/File/Standards/CTI%204501v0101.pdf
- ITU-R P.526-14 (diffraction; current P.526-15: https://www.itu.int/rec/R-REC-P.526), P.838-3 (rain), P.840-8 (fog), P.676-12 (gases)
- NIST FIPS 180-4: https://nvlpubs.nist.gov/nistpubs/FIPS/NIST.FIPS.180-4.pdf ; FIPS 203: https://nvlpubs.nist.gov/nistpubs/FIPS/NIST.FIPS.203.pdf ; FIPS 204: https://nvlpubs.nist.gov/nistpubs/FIPS/NIST.FIPS.204.pdf ; FIPS 205: https://nvlpubs.nist.gov/nistpubs/FIPS/NIST.FIPS.205.pdf ; FIPS 206 status: https://csrc.nist.gov/projects/post-quantum-cryptography/post-quantum-cryptography-standardization and https://csrc.nist.gov/csrc/media/presentations/2025/fips-206-fn-dsa-(falcon)/images-media/fips_206-perlner_2.1.pdf
- SEC 1 v2.0: https://www.secg.org/sec1-v2.pdf ; SEC 4 v1.0: https://www.secg.org/sec4-1.0.pdf ; RFC 5639: https://www.rfc-editor.org/rfc/rfc5639.txt ; RFC 8032: https://www.rfc-editor.org/rfc/rfc8032.html ; draft-ietf-lamps-pq-composite-sigs-19: https://www.ietf.org/archive/id/draft-ietf-lamps-pq-composite-sigs-19.txt
- GPS Standard Positioning Service Performance Standard, 5th edition (April 2020), gps.gov
- NHTSA, Federal Motor Vehicle Safety Standards; V2V Communications, Docket NHTSA-2016-0126 (2017)
- FHWA Signal Timing Manual (2008) Ch. 5: https://ops.fhwa.dot.gov/publications/fhwahop08024/chapter5.htm ; FHWA Roundabouts: An Informational Guide, Ch. 4; FHWA Road Weather Management, "How Do Weather Events Impact Roads?": https://ops.fhwa.dot.gov/weather/q1_roadimpact.htm ; FHWA Traffic Flow Theory monograph Ch. 2 (Hall); FHWA-HRT-08-051 SSAM validation: https://www.fhwa.dot.gov/publications/research/safety/08051/ ; FHWA-HRT-08-050 SSAM user manual: https://www.fhwa.dot.gov/publications/research/safety/08050/08050.pdf
- CAMP VSC-A Final Report, DOT HS 811 492A: https://rosap.ntl.bts.gov/view/dot/43933 ; CAMP SCMS PoC EE Requirements and Specifications, Release 1.1 (2016): https://pronto-core-cdn.prontomarketing.com/2/wp-content/uploads/sites/2896/2019/04/SCMS_POC_EE_Requirements.pdf ; USDOT/Booz Allen SCMS Design and Analysis (2013): https://rosap.ntl.bts.gov/view/dot/32051/dot_32051_DS1.pdf

Industry documents
- C2C-CC RS 2037 Vehicle C-ITS station profile R1.6.10 (2026-07-24): https://www.car-2-car.org/fileadmin/documents/Basic_System_Profile/Release_1.6.10/C2CCC_RS_2037_Profile_R1610.pdf ; C2C-CC TR 2052 Survey on ITS-G5 CAM statistics (2018-12-20): https://www.car-2-car.org/fileadmin/documents/General_Documents/C2CCC_TR_2052_Survey_on_CAM_statistics.pdf ; C2C-CC TR 2050 Spectrum Needs (2020-02-28): https://www.car-2-car.org/fileadmin/documents/General_Documents/C2CCC_TR_2050_Spectrum_Needs.pdf
- 5GAA List of C-V2X Devices (April 2024); 5GAA "Update on C-V2X Deployment in China" (2019): https://5gaa.org/content/uploads/2019/05/03.-Update_on_C-V2X_Deployment_in_China.pdf ; 5GAA "V2X State of Play in China II" (2025)
- W. Whyte (Qualcomm), V2X IEEE 1609.2.1 status and deployment (2022-08-31): https://avstandard.or.kr/uploads/file_content/9f26bee8-2e55-4c38-9e41-198653b7b05f/_%EB%B0%9C%ED%91%9C%EC%9E%90%EB%A3%8C__Korea-1609.2-2022-08-31-v2_Qualcomm_William_Whyte.pdf
- NXP SAF5400 fact sheet: https://www.nxp.com/docs/en/fact-sheet/SAF5400V2XFSA4.pdf ; NXP SXF1800: https://www.nxp.com/products/SXF1800 ; Cohda MK5 OBU brief: https://cohdawireless.com/wp-content/uploads/2024/06/CW_DL-Product-Brief-sheet-MK5-OBU.pdf ; Cohda MK5 module datasheet v1.2.0: https://fccid.io/2AEGPMK5RSU/Users-Manual/User-Manual-2618067.pdf ; Cohda MK6C EVK brief: https://cohdawireless.com/wp-content/uploads/2024/06/CW_DL-Product-Brief-sheet-MK6C-EVK.pdf ; Unex OBU-201U specification (FCC filing): https://apps.fcc.gov/els/GetAtt.html?id=201591&x=. ; Unex OBU-301E/351U information sheets; Autotalks CRATON datasheet, CRATON2/SECTON: https://auto-talks.com/products/craton2/ , https://auto-talks.com/products/secton/ , FIPS policy #3556: https://csrc.nist.gov/CSRC/media/projects/cryptographic-module-validation-program/documents/security-policies/140sp3556.pdf ; Commsignia ITS-RS4 brief: https://omniair.org/wp-content/uploads/2025/10/Commsignia_ITS_RS4_ProductBrief_v.09.3_22042020.pdf ; Infineon AURIX TC3xx HSM training; Thales Luna Network HSM 7 brief: https://cpl.thalesgroup.com/sites/default/files/content/product_briefs/field_document/2020-04/thales-luna-network-7-hsm-pb-a.pdf
- Aumovio ARS 408: https://engineering-solutions.aumovio.com/components/ars-408/ ; TXC automotive TCXO: https://www.txccorp.com/en/product/crystal-oscillators/tcxo/ ; Rakon PPS-OCXO: https://www.rakon.com/products/families/ocxo-ocso/pps-ocxo ; MathWorks `gpsSensor`: https://www.mathworks.com/help/uav/ref/gpssensor-system-object.html
- Eclipse MOSAIC Cell simulator docs: https://eclipse.dev/mosaic/docs/simulators/network_simulator_cell/ ; ns-3 5G-LENA NR module docs: https://5g-lena.cttc.es/ ; NR V2X design doc: https://5g-lena.cttc.es/static/archive/NR_V2X_V0.1_doc.pdf ; nr-v2x-dev branch: https://gitlab.com/cttc-lena/nr
- Ookla via IEEE ComSoc Technology Blog (2025-12-16): https://techblog.comsoc.org/2025/12/16/ookla-fwa-speed-test-results-for-u-s-carriers-wireless-connectivity-performance-at-busy-airports/ ; USDOT CV Pilot reports: https://its.dot.gov/pilots/thea_cvp_wireless.htm , https://itskrs.its.dot.gov/2020-sc00466 , https://rosap.ntl.bts.gov/view/dot/74648 , https://c2smart.engineering.nyu.edu/nyc-connected-vehicle-pilot/

Map, terrain and mobility data
- OSM Foundation Produced Work guideline: https://osmfoundation.org/wiki/Licence/Community_Guidelines/Produced_Work_-_Guideline ; OSM Wiki Overpass API: https://wiki.openstreetmap.org/wiki/Overpass_API ; OSM Simple 3D Buildings: https://wiki.openstreetmap.org/wiki/Simple_3D_buildings ; osmnx: https://github.com/gboeing/osmnx
- Overture Maps attribution: https://docs.overturemaps.org/attribution/ ; Microsoft Global ML Building Footprints: https://github.com/microsoft/GlobalMLBuildingFootprints ; Google Open Buildings: https://sites.research.google/gr/open-buildings/ ; NASA SRTMGL1: https://www.earthdata.nasa.gov/data/catalog/lpcloud-srtmgl1-003 ; Copernicus DEM GLO-30 license (cached text)
- OGC CityGML: https://www.ogc.org/standard/citygml/ ; ASAM OpenDRIVE: https://www.asam.net/standards/detail/opendrive/ ; Lanelet2: https://github.com/fzi-forschungszentrum-informatik/Lanelet2 ; osm2streets: https://github.com/a-b-street/osm2streets
- SUMO: road networks https://sumo.dlr.de/docs/Networks/SUMO_Road_Networks.html ; vehicle type defaults https://sumo.dlr.de/docs/Vehicle_Type_Parameter_Defaults.html ; vehicle and route definitions https://sumo.dlr.de/docs/Definition_of_Vehicles,_Vehicle_Types,_and_Routes.html ; pedestrians https://sumo.dlr.de/docs/Simulation/Pedestrians.html ; netconvert options; Krauß dissertation https://sumo.dlr.de/pdf/KraussDiss.pdf
- PTV Vissim 2023 Wiedemann 99 help: https://cgi.ptvgroup.com/vision-help/VISSIM_2023_ENG/Content/4_BasisdatenSim/FahrverhaltensparameterFolgeverh_Wied99.htm ; PTV VISUM 2025 unsignalized nodes (HCM values): https://cgi.ptvgroup.com/vision-help/VISUM_2025_ENG/Content/1_Benutzermodell%20IV/1_5_Vorfahrtsgeregelte%20Knoten.htm ; WisDOT TEOpS 16-20 attachment 6.3 (not opened): https://wisconsindot.gov/dtsdManuals/traffic-ops/manuals-and-standards/teops/16-20att6.3.pdf ; NACTO signal cycle lengths (HTTP 403): https://nacto.org/publication/urban-street-design-guide/intersection-design-elements/traffic-signals/signal-cycle-lengths/ ; Gipps' model (Wikipedia): https://en.wikipedia.org/wiki/Gipps%27_model

Papers
- Treiber, Hennecke, Helbing, "Congested traffic states in empirical observations and microscopic simulations," Phys. Rev. E 62, 1805 (2000), arXiv:cond-mat/0002177
- Kesting, Treiber, Helbing, "Enhanced Intelligent Driver Model to access the impact of driving strategies on traffic capacity," Phil. Trans. R. Soc. A 368, 4585 (2010), arXiv:0912.3613, https://doi.org/10.1098/rsta.2010.0084
- Kesting, Treiber, Helbing, "General lane-changing model MOBIL for car-following models," TRR 1999, 86-94 (2007), https://doi.org/10.3141/1999-10
- Helbing, Molnár, "Social force model for pedestrian dynamics," Phys. Rev. E 51, 4282 (1995), arXiv:cond-mat/9805244
- Pei, Henderson, "Validation of OFDM model in ns-3," ns-3 technical note (2010); Sjöberg et al., "Measuring and using the RSSI of IEEE 802.11p"; Torrent-Moreno, Mittag, Santi, Hartenstein, "Vehicle-to-vehicle communication: fair transmit power control for safety-critical information," IEEE TVT 58(7) (2009)
- Abbas, Sjöberg, Karedal, Tufvesson, "A measurement based shadow fading model for vehicle-to-vehicle network simulations," Int. J. Antennas Propag. (2015), arXiv:1203.3370 ; Karedal, Czink, Paier, Tufvesson, Molisch, "Path loss modeling for vehicle-to-vehicle communications," IEEE TVT 60(1), 323-328 (2011), mirror https://wides.usc.edu/Updated_pdf/Path%20loss%20modeling%20for%20vehicle-to-vehicle%20communications.pdf ; Cheng, Henty, Stancil, Bai, Mudalige, "Mobile vehicle-to-vehicle narrow-band channel measurement and characterization of the 5.9 GHz DSRC frequency band," IEEE JSAC 25(8) (2007), not retrieved ; Sommer, Eckhoff, German, Dressler, "A computationally inexpensive empirical model of IEEE 802.11p radio shadowing in urban environments," WONS 2011 ; Boban, Vinhoza, Barros, Ferreira, Tonguz, "Impact of vehicles as obstacles in vehicular ad hoc networks," IEEE JSAC 29(1), 15-28 (2011) ; Boban, "Realistic and efficient channel modeling for vehicular networks," PhD thesis, arXiv:1405.1008 ; Boban, Meireles, Barros, Steenkiste, Tonguz, "TVR: tall vehicle relaying in vehicular networks" ; Meireles, Boban, Steenkiste, Tonguz, Barros, "Experimental study on the impact of vehicular obstructions in VANETs," IEEE VNC 2010 ; Martelli, Renda, Santi, "Measuring IEEE 802.11p performance for active safety applications in cooperative vehicular systems," VTC 2011 ; Bai, Krishnan, "Reliability analysis of DSRC wireless communication for vehicle safety applications," IEEE ITSC 2006 (bibliographic record only) ; Taliwal, Jiang, Mangold, Chen, Sengupta, "Empirical determination of channel characteristics for DSRC vehicle-to-vehicle communication," VANET '04 (bibliographic record only) ; Yin, Holland, ElBatt, Bai, Krishnan, "DSRC channel fading analysis from empirical measurement" (via arXiv:1808.00509, composite α-μ DSRC channel model)
- Garcia, Boban, Kousaridas, Şahin, "A tutorial on 5G NR V2X communications," IEEE COMST (2021), https://doi.org/10.1109/COMST.2021.3057017 , arXiv:2102.04538 ; Ali, Lagén, Giupponi, "On the impact of numerology in NR V2X Mode 2 with sensing and no-sensing resource selection," arXiv:2106.15303 (2021) ; Ali, Lagén, Giupponi, Rouil, "3GPP NR V2X Mode 2: overview, models and system-level evaluation," IEEE Access (2021), https://doi.org/10.1109/ACCESS.2021.3090855
- Molina-Masegosa, Gozálvez, "LTE-V for sidelink 5G V2X vehicular communications," IEEE VT Mag 12(4), 30-39 (2017), https://doi.org/10.1109/MVT.2017.2752798 , https://dspace.umh.es/bitstream/11000/5078/1/11-LTE-V%20for%20Sidelink....pdf ; Molina-Masegosa, Gozálvez, "System level evaluation of LTE-V2V Mode 4 communications and its distributed scheduling," VTC-Spring 2017 ; Gonzalez-Martin, Sepulcre, Molina-Masegosa, Gozálvez, "Analytical models of the performance of C-V2X Mode 4 vehicular communications," IEEE TVT (2019), arXiv:1807.06508, code https://github.com/msepulcre/C-V2X ; Bazzi, Cecchini, Zanella, Masini, "Study of the impact of PHY and MAC parameters in 3GPP C-V2V Mode 4," IEEE Access 6, 71685-71698 (2018), https://doi.org/10.1109/ACCESS.2018.2883401 , arXiv:1807.10699 ; Bazzi et al., "Survey and perspectives of vehicular Wi-Fi versus sidelink cellular-V2X in the 5G era," Future Internet 11(6):122 (2019), not retrievable ; Todisco, Bartoletti, Campolo, Molinaro, Berthet, Bazzi, "Performance analysis of sidelink 5G-V2X Mode 2 through an open-source simulator," IEEE Access 9, 145648-145661 (2021), https://doi.org/10.1109/ACCESS.2021.3121151 ; McCarthy, Burbano-Abril, Rangel Licea, O'Driscoll, "OpenCV2X," arXiv:2103.13212 ; Mansouri, Martinez, Härri, WONS 2019: https://dl.ifip.org/db/conf/wons/wons2019/130.pdf ; Toghi et al., arXiv:1904.00071 ; Qualcomm R1-1611594; Huawei R1-160284
- Rostami, Krishnan, Gruteser, "V2V safety communication scalability based on the SAE J2945/1 standard" (2018): https://winlab.rutgers.edu/~rostami/files/pdf/rostami2018v2v.pdf ; Kenney, "Dedicated short-range communications (DSRC) standards in the United States" (tutorial)
- Twardokus, Bindel, Rahbari, McCarthy, "When cryptography needs a hand: practical post-quantum authentication for V2V communications," NDSS 2024: https://www.ndss-symposium.org/wp-content/uploads/2024-267-paper.pdf , ePrint 2022/483, code https://github.com/twardokus/pq-v2verifier ; Bindel, McCarthy, Rahbari, Twardokus, "Suitability of 3rd round signature candidates for vehicle-to-vehicle communication," NIST PQC Conf. 2021: https://csrc.nist.gov/CSRC/media/Presentations/suitability-of-3rd-round-signature-candidates-for/images-media/session-5-bindel-suitability-vehicle.pdf ; VehicleSec 2024 demo: https://www.ndss-symposium.org/wp-content/uploads/vehiclesec2024-7-demo.pdf ; Yoshizawa, Preneel, "On handling of certificate digest in V2X communication": https://cosicdatabase.esat.kuleuven.be/backend/publications/files/conferencepaper/3530 ; Cominetti et al., "Faster verification of V2X BSM messages via message chaining," ePrint 2022/133: https://eprint.iacr.org/2022/133.pdf ; Simplicio et al., ACPC, ePrint 2018/324: https://eprint.iacr.org/2018/324.pdf ; Brecht et al., "A security credential management system for V2X communications," arXiv:1802.05323 ; arXiv:2608.05087 (PQ signatures in C-V2X) ; Ostendorf, Garlichs, Wolf, arXiv:2506.22052 ; Islam et al., PASS, arXiv:1907.05284
- Dilithium round-3 specification: https://pq-crystals.org/dilithium/data/dilithium-specification-round3-20210208.pdf ; Pornin, "New efficient, constant-time implementations of Falcon," ePrint 2019/893: https://eprint.iacr.org/2019/893.pdf ; falcon-sign.info ; SPHINCS+ r3.1 specification: https://sphincs.org/data/sphincs+-r3.1-specification.pdf ; pqm4 benchmarks: https://raw.githubusercontent.com/mupq/pqm4/master/benchmarks.csv ; NIST 4th PQC conference ARM benchmarking paper: https://csrc.nist.gov/csrc/media/Events/2022/fourth-pqc-standardization-conference/documents/papers/benchmarking-and-analysiing-nist-pqc-lattice-based-pqc2022.pdf ; Berger, Lemoudden, Buchanan, "Post quantum migration of Tor," arXiv:2503.10238 ; Emill/P256-Cortex-M4: https://github.com/Emill/P256-Cortex-M4 ; wolfSSL vs mbedTLS benchmark: https://www.wolfssl.com/wolfssl-vs-mbedtls-an-apples-to-apples-benchmark-across-intel-arm-cortex-a-and-cortex-m-and-risc-v-targets/ ; OpenSSL speed gists: https://gist.github.com/HimaJyun/f05d3017dfb05a4ccb0def010bb2c91a ; pqc-forum FIPS 206 status: https://groups.google.com/a/list.nist.gov/g/pqc-forum/c/1HXzjlMUU6Y
- Narayanan et al., "A first look at commercial 5G performance on smartphones," WWW'20, https://doi.org/10.1145/3366423.3380169 ; Coll-Perales et al., "End-to-end V2X latency modeling and analysis in 5G networks," IEEE TVT (2022), https://doi.org/10.1109/TVT.2022.3224614 , arXiv:2201.06082 ; Cauchi et al., MASA living lab, arXiv:2606.13292
- Puñal, Aguiar, Gross, "In VANETs we trust? Characterizing RF jamming in vehicular networks," ACM VANET 2012: https://www.jamesgross.org/wp-content/uploads/2016/01/Punal_Aguiar_Gross_VANET_12.pdf ; "Securing V2X communications" survey, arXiv:2003.07191 ; GNSS spoofing threat for V2X, arXiv:2606.20215 ; Vehicular wireless positioning survey, arXiv:2601.20547
- Reid, Pervez, Ibrahim, Houts, Pandey, Alla, Hsia, "Standalone and RTK GNSS on 30,000 km of North American highways," ION GNSS+ 2019 ; Wen, Hsu et al., 3D-LiDAR-aided GNSS NLOS mitigation and GNSS-RTK (UrbanNav), Hong Kong PolyU
- Kamel et al., "Simulation framework for misbehavior detection in vehicular networks," IEEE TVT 69(6), 6631-6643 (2020), https://doi.org/10.1109/TVT.2020.2984878 ; F2MD: https://github.com/josephkamel/F2MD ; veins-f2md: https://github.com/josephkamel/veins-f2md ; van der Heijden, Lukaseder, Kargl, "VeReMi," SecureComm 2018 ; Kamel et al., "VeReMi Extension," ICC 2020 ; Hermann et al., "VeReMi NextGen," VNC 2026

Tools and libraries
- rasn: https://github.com/librasn/rasn ; rasn-its: https://lib.rs/crates/rasn-its ; rasn-compiler: https://github.com/librasn/compiler ; ETSI Forge legal notice: https://forge.etsi.org/index.php/legal-matters ; asn1c: https://github.com/vlm/asn1c ; USDOT asn1_codec: https://github.com/usdot-jpo-ode/asn1_codec ; pycrate: https://github.com/pycrate-org/pycrate ; asn1tools: https://github.com/eerimoq/asn1tools ; Vanetza: https://www.vanetza.org/recipes/generate-asn1/ ; Wireshark WSMP dissector: https://raw.githubusercontent.com/wireshark/wireshark/master/epan/dissectors/packet-wsmp.c
- liboqs: https://github.com/open-quantum-safe/liboqs ; PQClean: https://github.com/PQClean/PQClean ; pqcrypto (Rust): https://github.com/rustpq/pqcrypto ; RustCrypto p256: https://github.com/RustCrypto/elliptic-curves ; pyca/cryptography: https://github.com/pyca/cryptography ; ecqv crate: https://github.com/Abdk4Moura/ecqv ; V2Verifier: https://github.com/twardokus/v2verifier
- ns-3 `wifi-phy.cc`, `propagation-loss-model.cc`; Veins `Mac1609_4.ned`, `PhyLayer80211p.ned`, `TwoRayInterferenceModel.cc`; WiLabV2Xsim PER tables (`PER_table.mat`, `percurvesgen.m`); cached `nist_per.py` (reproduction of Pei and Henderson Table I)

Legacy reference
- `src/scms_sim_ref/mock_pipeline/run.py`, `roads.py`, `osm.py` at commit 2832d63 (frozen as `legacy/scms_sim_ref/`), cited by line number as `code (legacy)`.
