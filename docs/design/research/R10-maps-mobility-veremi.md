# R10 — Maps/3D Data, Mobility Parameters, and VeReMi/F2MD/ETSI Fact Sheet

Rules applied: every row cites a source + access date. Cached local files (from this session's research scratchpad) are cited as `local cache: <filename>` with the retrieval timestamp shown in `ls -la` (2026‑09‑17) plus the paper's own identity (DOI/arXiv/standard number) where known. Fresh web lookups are cited with URL + access date **2026‑09‑18**. Anything not found after a genuine attempt is marked **UNVERIFIED** — no invented numbers.

---

## TOPIC A — Map / 3D Data and Licences

### A1. OSM ODbL — Produced Work vs. Derivative Database

| Item | Value | Source |
|---|---|---|
| ODbL "Produced Work" definition | "a work (such as an image, audiovisual material, text, or sounds) resulting from using the whole or a Substantial part of the Contents" of the database | OSM Foundation, Licence/Community Guidelines/Produced Work – Guideline, https://osmfoundation.org/wiki/Licence/Community_Guidelines/Produced_Work_-_Guideline (accessed 2026‑09‑18) |
| Decision rule: database vs. produced work | "If your project's published result is intended for extraction of original data, it is a database." Otherwise it is a Produced Work. | same, accessed 2026‑09‑18 |
| Share-Alike still applies to the *source* | Even when the output is a Produced Work, "the underlying database has to be published as well" (or its modifications), per ODbL §4.6 | same, accessed 2026‑09‑18 |
| Typically Produced Works | PNG/JPG/PDF/SVG raster images, printed maps | same |
| Typically NOT Produced Works | Database dumps such as Planet exports | same |
| Whether an exported lane-geometry dataset (e.g. a simulator's `net.xml`/`.osm`-derived road graph) is a Produced Work or a Derivative Database | **UNVERIFIED / ambiguous** — the guideline explicitly states this is unresolved and defers to community forum discussion. Practical implication for this project: an exported dataset that preserves OSM's geometry/topology at the same granularity (i.e., could be used to reconstruct the source data) is very likely a **Derivative Database**, not a Produced Work, and inherits full ODbL Share-Alike + attribution obligations. | OSM Foundation guideline, accessed 2026‑09‑18 (interpretation flagged as ours, not an ODbL legal ruling) |
| Can a CC‑BY‑4.0 dataset include OSM‑derived geometry? | **No, not as a substitute licence for the OSM-derived portion.** ODbL is a copyleft (share-alike) licence; relicensing OSM-derived *database* content under CC‑BY‑4.0 alone does not satisfy ODbL §4.6 obligations (attribution + share-alike + keep-open). A produced-work image *can* be published under any licence including CC‑BY, provided the underlying OSM data/attribution notice is still made available per §4.6, but a machine-readable derivative geodatabase generally must remain ODbL (or dual/multi-licensed with ODbL retained). | Derived from OSM Foundation guideline + ODbL §4.6 text, accessed 2026‑09‑18 — **treat as UNVERIFIED for a formal legal opinion**, but consistent with common OSM community practice (see also Overture's own approach below, which keeps ODbL on OSM-derived themes rather than relicensing them). |

### A2. Overpass API limits

| Item | Value | Source |
|---|---|---|
| Casual/public use limit (overpass-api.de) | < 10,000 queries/day and < 1 GB downloaded/day | OSM Wiki, Overpass API, https://wiki.openstreetmap.org/wiki/Overpass_API (accessed 2026‑09‑18) |
| "Regular application" limit | ~1/100th of casual limits (≈100 queries/day, ≈10 MB/day) | same |
| Default query timeout | 3 minutes (extendable to 900s / 15 min via `[timeout:900]`) | same |
| Rate-limit handling | On HTTP 429/406, back off ≥30s before retry; add identifying `User-Agent`; avoid parallel requests; prefer regional extracts for bulk needs | same |
| Commercial use | Requires self-hosting or a paid provider (e.g. Geofabrik, Tracestrack) | same |

### A3. osmnx

| Item | Value | Source |
|---|---|---|
| Licence | MIT | github.com/gboeing/osmnx, accessed 2026‑09‑18 |
| Purpose | Python package to download/model/analyze/visualize OSM street networks and geospatial features | same |

### A4. OSM building height / levels tags & coverage

| Item | Value | Source |
|---|---|---|
| `height` tag | Distance from lowest ground-contact point to top of roof, excluding antennas/spires/rooftop equipment | OSM Wiki, Simple 3D Buildings, https://wiki.openstreetmap.org/wiki/Simple_3D_buildings (accessed 2026‑09‑18) |
| `building:levels` | Number of above-ground floors (excludes roof levels); intended to **supplement**, not replace, `height` | same |
| `min_height` | Ground clearance for elevated structures (e.g. bridge 3 m tall, 10 m up → `min_height=10, height=13`) | same |
| `building:min_level` | Underground analogue of `min_height` for skipped levels | same |
| Level→metre default conversion factor used by renderers | **UNVERIFIED** — the OSM wiki page fetched does not state a standard factor (common renderer conventions of ~3 m/level exist informally but are not documented on this page) | same, accessed 2026‑09‑18 |
| Tag coverage/completeness across the OSM database | **UNVERIFIED** — not quantified on the fetched page; known qualitatively to be sparse/inconsistent globally (no citable number obtained this session) | — |

### A5. Overture Maps

| Item | Value | Source |
|---|---|---|
| Licence — Base, Buildings, Divisions, Transportation themes | ODbL ("© OpenStreetMap contributors. Available under the Open Database License") — **ODbL obligations are preserved**, not relicensed away, for OSM-derived themes | Overture Maps Docs, Attribution, https://docs.overturemaps.org/attribution/ (accessed 2026‑09‑18) |
| Licence — Places theme | CDLA Permissive 2.0 (multi-contributor: Meta, Microsoft, Foursquare, etc.) | same |
| Licence — Addresses theme | Varies by country; predominantly CC BY 4.0 / CC0 / public domain | same |
| Required attribution (OSM-derived) | "© OpenStreetMap contributors, Overture Maps Foundation" | same |
| General citation | "Overture Maps Foundation, overturemaps.org" | same |
| Themes available | Base, Buildings, Divisions, Transportation, Places, Addresses | docs.overturemaps.org (accessed 2026‑09‑18) |
| Whether building heights included | **UNVERIFIED in this session** — the fetched landing page confirmed a Buildings theme exists but did not enumerate the height attribute; the Overture schema reference (not fetched) is the authoritative place to confirm | docs.overturemaps.org, accessed 2026‑09‑18 |

### A6. Microsoft Global ML Building Footprints

| Item | Value | Source |
|---|---|---|
| Licence | CDLA Permissive 2.0 | github.com/microsoft/GlobalMLBuildingFootprints, accessed 2026‑09‑18 |
| Coverage | ~1.4 billion footprints, 225 regions, 30,340 tiles, imagery 2014–2024 | same |
| Format | Line-delimited GeoJSON, `.csv.gz`, EPSG:4326 | same |
| Height data | Yes — ≈174 million footprints have neural-network-estimated height (metres); `-1` = no estimate | same |
| Confidence / false-positive rate | Confidence score 0–1; false-positive rate ~0.1–2.2% by region (sampled validation) | same |

### A7. Google Open Buildings

| Item | Value | Source |
|---|---|---|
| Licence | Dual: CC‑BY‑4.0 **or** ODbL v1.0 (user's choice) | sites.research.google/gr/open-buildings/, accessed 2026‑09‑18 |
| Coverage | ~58M km², Africa/South Asia/Southeast Asia/Latin America/Caribbean, 145+ countries | same |
| Volume | 1.8 billion building detections | same |
| Format | CSV sharded by S2 cell level 4; fields: lat/lon, area (m²), confidence (0.65–1.0), WKT polygon, Plus Code | same |
| Height | **Not included** — rooftop footprint only, no vertical dimension | same |

### A8. SRTM

| Item | Value | Source |
|---|---|---|
| Licence | Open/public — "openly shared, without restriction, in accordance with the EOSDIS Data Use and Citation Guidance" (citation requested, not legally required) | NASA Earthdata, SRTM GL1, https://www.earthdata.nasa.gov/data/catalog/lpcloud-srtmgl1-003 (accessed 2026‑09‑18) |
| Resolution (SRTMGL1) | 30 m × 30 m (≈1 arc-second) | same |
| Vertical accuracy | **UNVERIFIED in this session** — page defers to Rodriguez et al. (2006) "known issues" reference, not independently fetched | same, accessed 2026‑09‑18 |

### A9. Copernicus GLO‑30 DEM licence (COP‑DEM‑GLO‑30‑F)

| Item | Value | Source |
|---|---|---|
| Cost | Free of charge | Copernicus WorldDEM‑30 licence text, local cache `copdem_license.pdf.txt` (retrieved 2026‑09‑17); document = "Licence for Copernicus DEM instance COP-DEM-GLO-30-F Global 30m Full, Free & Open" |
| Rights granted | Reproduction, distribution, communication to the public, adaptation/modification/combination — worldwide, unlimited in time | same |
| Attribution notice (unmodified) | "© DLR e.V. 2010‑2014 and © Airbus Defence and Space GmbH 2014‑2018 provided under COPERNICUS by the European Union and ESA; all rights reserved." | same |
| Attribution notice (modified) | "produced using Copernicus WorldDEM‑30 © DLR e.V. 2010‑2014 and © Airbus Defence and Space GmbH 2014‑2018 provided under COPERNICUS by the European Union and ESA; all rights reserved." | same |
| Liability disclaimer requirement | Redistributors must add: "The organisations in charge of the Copernicus programme by law or by delegation do not incur any liability for any use of the Copernicus WorldDEM‑30" | same |
| IPR | Airbus Defence & Space GmbH retains IP; Licensor (EU/ESA framework) only sublicenses; no trademark rights granted beyond stated use | same |
| Warranty | Provided "as is", no warranty, user bears risk and indemnifies Licensor/Provider | same |
| Higher-res WorldDEM‑10 | Explicitly **excluded** from this licence — separate licence required | same |

### A10. CityGML

| Item | Value | Source |
|---|---|---|
| What it is | OGC open standard: "conceptual model and exchange format for the representation, storage and exchange of virtual 3D city models" | ogc.org/standard/citygml/, accessed 2026‑09‑18 |
| Licence | Open standard, freely accessible spec (v1.0, 2.0, 3.0 all published as OGC International Standards) | same |
| Road/lane-level detail | **Not its focus** — CityGML targets buildings/city objects (BIM, urban planning, facility management); no dedicated road-lane/traffic-signal data structures found on this page | same |

### A11. ASAM OpenDRIVE

| Item | Value | Source |
|---|---|---|
| Cost / access | Free to download and use; requires accepting ASAM Terms & Conditions; no membership fee or royalty to implement | asam.net/standards/detail/opendrive/, accessed 2026‑09‑18 |
| Format | XML (`.xodr`) | same |
| Coverage | Road reference-line geometry, lanes (perpendicular to reference line) with elevation profiles, junctions (entry + connecting roads), objects/roadmarks/signals, lane linkage for routing | same |
| Elevation | Yes — elevation profiles are part of the lane/road model | same |
| Target use | ADAS/AV simulation and testing | same |

### A12. SUMO `net.xml` structure

| Element | Attributes / role | Source |
|---|---|---|
| `<edge>` | `id`, `from`/`to` (junctions), `priority`, `function` (normal/internal/connector/crossing/walkingarea) | sumo.dlr.de/docs/Networks/SUMO_Road_Networks.html, accessed 2026‑09‑18 |
| `<lane>` (child of edge) | `id`, `index` (0 = rightmost), `speed` (m/s), `length` (m), `shape` (polyline, 2D/3D coords) | same |
| `<junction>` | `id`, `x`/`y`, `incLanes`, `intLanes` (internal lanes), `shape` (polygon) | same |
| `<connection>` | `from`/`to` edge, `fromLane`/`toLane`, `via` (internal lane), `dir` (turn code), `state`, `linkIndex` (→ tlLogic signal index) | same |
| `<tlLogic>` | `id`, `programID`, `offset`, child `<phase>` with `duration` + `state` (bitstring, one char per link, left-to-right = link 0..n) | same |
| `<request>` (per junction) | `index`, `response` (priority bitstring), `foes` (conflict bitstring), `cont` (internal-junction wait flag) | same |
| Licence of SUMO itself | EPL‑2.0 (Eclipse Public License) — SUMO project is Eclipse-hosted (general knowledge, not independently re-verified this session — **flag as UNVERIFIED** if exact wording is load-bearing) | sumo.dlr.de (general project knowledge) |
| netconvert import formats | plain-XML, OSM, VISUM, Vissim, OpenDRIVE, MATSim, SUMO net.xml, Shapefile, RoboCup, DlrNavteq/GDF | local cache `netconvert.md` (retrieved 2026‑09‑17) |
| netconvert export formats | SUMO net.xml/plain-XML, OpenDRIVE, MATSim, DlrNavteq, Amitran | same |
| Sidewalk import | `--osm.sidewalks` imports OSM sidewalk tags directly | local cache `pedestrians.md` |
| Crossing generation | `--crossings.guess` heuristic; crossings only auto-placed at TLS junctions above `crossings.guess.speed-threshold` (>50 km/h approach) | same |
| Walking areas | Auto-generated whenever `--crossings.guess` or any user crossing exists, or via `--walkingareas` | same |

### A13. Lanelet2

| Item | Value | Source |
|---|---|---|
| Licence | BSD 3-Clause | github.com/fzi-forschungszentrum-informatik/Lanelet2, accessed 2026‑09‑18 |
| Data model | "Lanelets" (drivable-area primitives) built from points + linestrings; edits to a shared point propagate to all referencing objects (topological consistency) | same |
| Lane connectivity | Native lane-change/routing support via `lanelet2_routing` | same |
| Signals / traffic rules | `lanelet2_traffic_rules` package interprets regulatory elements (traffic signs/lights) encoded in the map | same |
| Elevation | Explicit 2D **and** 3D support | same |

### A14. osm2streets

| Item | Value | Source |
|---|---|---|
| Purpose | Rust library + tools transforming raw OSM tags into a simplified lane-level street-network schema (roads = list of lanes left-to-right with type/direction/width; intersections = polygon areas) | local cache `osm2streets.md` (retrieved 2026‑09‑17); project: github.com/a-b-street/osm2streets |
| Transformations offered | Collapse unnecessary intersections; merge dual-carriageway "sausage links"; merge dog-leg intersections; snap parallel cycletracks/footways to main road | same |
| Planned (not yet implemented as of README) | Turning movements, crosswalks, bike boxes/advanced stop lines, modal filters, routing, isochrones, GPS map-matching | same |
| Output | GeoJSON (lane + intersection polygons, lane markings) | same |
| Licence | **UNVERIFIED this session** — not stated in the cached README excerpt; the parent project A/B Street is Apache-2.0 (general knowledge, not independently re-confirmed here) | local cache `osm2streets.md` |
| Bindings | JS (WASM), Python, Java (in progress), C++ planned | same |

### A15. Comparison — candidate canonical lane-level formats

| Format | Licence | Lane connectivity | Signals | Elevation | Sidewalks/crossings | Buildings | Tooling |
|---|---|---|---|---|---|---|---|
| **SUMO net.xml** | EPL-2.0 (SUMO project; format itself has no separate licence) | Explicit `<connection>` + internal lanes (`<junction intLanes>`) | `<tlLogic>` phases keyed by `linkIndex` | Supported via z-coord in lane `shape` + elevation import (`--heightmap.*`) | First-class: sidewalk lanes, `crossing`/`walkingarea` lane functions | Not modeled (separate `.poly.xml` for polygons/buildings, not integrated network geometry) | netconvert, netedit, duarouter, TraCI, huge Python toolset — most mature open microsimulation tooling |
| **ASAM OpenDRIVE** | Free to use, ASAM Terms & Conditions (not an OSI open-source licence; spec download is free) | Lane linkage across roads + junction connecting-roads | Signals as positioned objects w/ signal-timing reference | Elevation profiles native to road reference line | Roadmarks/objects can represent them; not first-class pedestrian network model like SUMO | Not covered (road network standard only) | Used by many commercial AV sim tools (CARLA, esmini, VTD, dSPACE, etc.) |
| **Lanelet2** | BSD-3-Clause | Native via lanelet routing graph, regulatory elements | `lanelet2_traffic_rules` module | 2D and 3D native | Can be modeled as lanelets (subtype-dependent) | Not covered | Autoware ecosystem, JOSM-adjacent tooling |
| **osm2streets** | Unverified this session (README doesn't state; used by Apache-2.0 A/B Street) | Simplified road=lane-list model; turning movements "planned", not yet implemented | Not implemented (planned) | Not addressed in README | Snaps footways/cycletracks; crossings "planned" | Not covered | JS/WASM + Python bindings, StreetExplorer web app |
| **Custom (project-specific)** | Whatever project chooses | Design freedom, but must be built from scratch | Design freedom | Design freedom | Design freedom | Could integrate buildings/DEM natively if designed for it | No existing ecosystem — full build cost |

Overall read for a **V2X simulator**: SUMO net.xml is the most mature for lane-level connectivity + signals + pedestrian infrastructure + tooling maturity today; OpenDRIVE is the closest thing to an AV-industry-standard interchange format (best 3rd-party sim compatibility) but weaker on pedestrian/ped-crossing modeling; Lanelet2 is strongest on 3D/elevation + regulatory-element modeling but weakest on ready-made import tooling from OSM; osm2streets is the most promising *simplification layer* on top of raw OSM but is pre-1.0 / missing signals and turn restrictions as of the README reviewed.

---

## TOPIC B — Mobility Parameters

### B1. Intelligent Driver Model (IDM) — Treiber, Hennecke & Helbing (2000)

Source: Treiber, Hennecke & Helbing, "Congested traffic states in empirical observations and microscopic simulations," Phys. Rev. E 62, 1805 (2000); arXiv:cond-mat/0002177. Local cache `idm2000.pdf.txt` (retrieved 2026‑09‑17).

| Parameter | Symbol | Value used in paper | Notes |
|---|---|---|---|
| Desired velocity | v₀ | 120 km/h | freeway calibration |
| Safe time headway | T | 1.6 s | "slightly lower than suggested by German authorities (1.8 s)" |
| Max acceleration | a | 0.73 m/s² | ≈ 0–100 km/h in 45 s |
| Comfortable (desired) deceleration | b | 1.67 m/s² | "consistent with empirical investigations" |
| Jam distance (min gap) | s₀ | (paper sets s₁=0; s₀ implied ~2 m in worked examples) | |
| Acceleration exponent | δ | 4 | throughout the paper |
| Vehicle length | l | 5 m | |
| Equilibrium gap | sₑ(v) = s₀ + vT (high-density limit) | — | Newell-type limit |
| Desired minimum gap | s*(v,Δv) = s₀ + s₁√(v/v₀) + Tv + vΔv/(2√(ab)) | — | s₁ set to 0 in the base paper |
| Jam density (calibrated) | ρ_jam | 140 veh/km | linearly-stable congested flow has Q_jam ≈ 0 for this parameter set |
| Convective-stability flow threshold | Q_cv | 1050 veh/h | congested traffic convectively stable below this |
| Jam propagation velocity | v_g | ≈ −15 km/h | matches widely-cited stop-and-go wave speed |
| Capacity drop (empirical, cited) | ~20% | typical order of magnitude at freeway bottleneck breakdown |
| Numerical performance | ~10⁵ vehicles in real time on a "usual workstation" (c. 2000 hardware) | |

### B2. "Enhanced IDM" / ACC model (sometimes referred to as IIDM in later literature) — Kesting, Treiber & Helbing (2010)

Source: Kesting, Treiber & Helbing, "Enhanced Intelligent Driver Model to Access the Impact of Driving Strategies on Traffic Capacity," Phil. Trans. R. Soc. A 368, 4585 (2010); arXiv:0912.3613. Local cache `iidm2010.txt` (retrieved 2026‑09‑17). **Note:** this cached paper is the IDM→CAH→"ACC model" enhancement paper (fixes cut-in overreaction), not the distinct Treiber & Kanagaraj "IIDM" formulation — flagged so the naming isn't confused downstream.

| Parameter | Car | Truck |
|---|---|---|
| Desired speed v₀ | 120 km/h | 85 km/h |
| Free-accel exponent δ | 4 | 4 |
| Desired time gap T | 1.5 s | 2.0 s |
| Jam distance s₀ | 2.0 m | 4.0 m |
| Max acceleration a | 1.4 m/s² | 0.7 m/s² |
| Desired deceleration b | 2.0 m/s² | 2.0 m/s² |
| Coolness factor c (ACC-only) | 0.99 | 0.99 (realistic range c ∈ [0.95, 1.00]) |

Key findings: max free-flow-capacity sensitivity ≈ **0.3% throughput gain per 1% ACC penetration** (range 0.32–0.42 per the kernel-regression fits); dynamic (post-breakdown) capacity gain ≈ **0.24%/1%** ACC at low penetration, over-proportional at higher penetration; capacity drop (max free flow vs. dynamic/outflow capacity) realistic range **5–20%** (citing Kerner & Rehborn 1996; Cassidy & Bertini 1999), and this paper's own simulations found **5–15%**. Traffic-adaptive ACC strategy matrix (multipliers λ_T, λ_a, λ_b applied to T/a/b by traffic state): Free traffic (1,1,1); Upstream jam front (1,1,0.7); Congested traffic (1,1,1); Downstream jam front (0.5,2,1); Bottleneck (0.7,1.5,1).

### B3. MOBIL lane-changing model — Kesting, Treiber & Helbing (2007)

Source: Kesting, Treiber & Helbing, "General Lane-Changing Model MOBIL for Car-Following Models," Transportation Research Record 1999, 86–94 (2007); DOI 10.3141/1999-10. Local cache `mobil.pdf.txt` (retrieved 2026‑09‑17).

| Parameter | Value used in paper's simulations | Meaning |
|---|---|---|
| Politeness factor p | swept 0 → 1 (main behavioral knob) | 0 = purely egoistic, 1 = "ideal MOBIL" (maximize summed acceleration of ego+both followers), negative = malicious |
| Changing threshold Δa_th | 0.1 m/s² | prevents lane changes for marginal advantage |
| Max safe deceleration b_safe | 4 m/s² (well below physical max ≈ 9 m/s² on dry roads) | safety criterion: new follower's deceleration after the change must not exceed this |
| Right-lane bias Δa_bias (asymmetric/European rules) | 0.3 m/s² | must exceed Δa_th or vehicles won't return to an empty right lane |
| Underlying car-following model in the simulation study | IDM, T=1.2 s, a=1.5 m/s², b=2 m/s², s₀=2 m; cars v₀=120 km/h (l=4 m), trucks v₀=80 km/h (l=12 m), 20% truck fraction, ±20% speed heterogeneity | |
| Safety criterion | ã_n ≥ −b_safe (new follower's post-change deceleration) | |
| Incentive criterion (symmetric, p=1 special case) | ã_c + ã_n + ã_o > a_c + a_n + a_o | literal "Minimizing Overall Braking Induced by Lane Changes" |
| Peak lane-change rate found | ~1100–1400 changes/h/km at p=0 vs. ~450–600 at p=1 (symmetric vs. asymmetric rules) | at intermediate density 10–15 veh/km/lane |

### B4. SUMO Krauss / vType defaults per vClass

Source: SUMO docs, "Vehicle Type Parameter Defaults." Local cache `vtype_defaults.md` (retrieved 2026‑09‑17); canonical URL https://sumo.dlr.de/docs/Vehicle_Type_Parameter_Defaults.html.

| vClass | length (m) | width (m) | height (m) | mass (kg) | minGap (m) | accel (m/s²) | decel (m/s²) | emergencyDecel (m/s²) | maxSpeed (km/h) | speedDev |
|---|---|---|---|---|---|---|---|---|---|---|
| passenger | 5 | 1.8 | 1.5 | 1500 | 2.5 | 2.6 | 4.5 | 9 | 200 | 0.1 |
| emergency | 6.5 | 2.16 | 2.86 | 5000 | 2.5 | 2.6 | 4.5 | 9 | 200 | 0 |
| delivery | 6.5 | 2.16 | 2.86 | 5000 | 2.5 | 2.6 | 4.5 | 9 | 200 | 0.05 |
| truck | 7.1 | 2.4 | 2.4 | 4500 | 2.5 | 1.3 | 4 | 7 | 130 | 0.05 |
| trailer | 16.5 | 2.55 | 4 | 13000 | 2.5 | 1.0 | 4 | 7 | 130 | 0.05 |
| bus | 12 | 2.5 | 3.4 | 12000 | 2.5 | 1.2 | 4 | 7 | 85 | 0.1 |
| coach | 14 | 2.6 | 4.0 | 25000 | 2.5 | 2.0 | (=bus: 4) | (=bus: 7) | 100 | 0.05 |
| motorcycle | 2.2 | 0.9 | 1.5 | 200 | 2.5 | 6 | 10 | 10 | 200 | 0.1 |
| moped | 2.1 | 0.8 | 1.7 | 80 | 2.5 | 1.1 | 7 | 10 | 45 | 0.1 |
| bicycle | 1.6 | 0.65 | 1.7 | 10 | 0.5 | 1.2 | 3 | 7 | 50 (desired 20) | 0.1 |
| pedestrian | 0.215 | 0.478 | 1.719 | 70 | 0.25 | 1.5 | 2 | 5 | 37.58 (desired 5) | 0.1 |
| scooter (e-scooter) | 1.2 | 0.5 | 1.7 | 10 | 0.5 | 1.2 | 3 | 7 | 25 (desired 20) | 0.1 |

Default car-following model = **Krauss**; default lane-changing model = **LC2013** (SUMO docs, `vehdef.md`, retrieved 2026‑09‑17).

### B5. SUMO LC2013 lane-change parameters (defaults)

Source: local cache `vehdef.md` (SUMO "Definition of Vehicles, Vehicle Types, and Routes" docs), retrieved 2026‑09‑17; canonical URL https://sumo.dlr.de/docs/Definition_of_Vehicles,_Vehicle_Types,_and_Routes.html.

| Parameter | Default | Range | Meaning |
|---|---|---|---|
| lcStrategic | 1.0 | [0,∞), −1 disables | eagerness for strategic (route-following) lane changes |
| lcCooperative | 1.0 | [0,1], −1 disables | willingness to cooperate/yield |
| lcSpeedGain | 1.0 | [0,∞) | eagerness to change lanes to gain speed |
| lcKeepRight | 1.0 | [0,∞) | eagerness to obey keep-right rule |
| lcContRight | 1.0 | [0,1] | prob. of choosing rightmost lane on lane increase |
| lcOvertakeRight | 0 | [0,1] | prob. of violating no-overtake-on-right rule |
| lcOpposite | 1.0 | [0,∞) | eagerness for opposite-direction overtaking |
| lcStrategicLookahead | 3000.0 m | [0,∞) | lookahead distance for best-lane computation |
| lcSpeedGainRight | 0.1 | [0,∞) | asymmetry factor, right vs left speed-gain threshold |
| lcSpeedGainLookahead | 0 (LC2013) / 5 (SL2015) | [0,∞) | anticipation time (s) for slow-down |
| lcSpeedGainRemainTime | 20 s | [0,∞) | min time on new lane after a speed-gain change |
| lcSpeedGainUrgency | 50 | [0,∞) | threshold marking a speed-gain change as urgent |
| lcAssertive | 1 | positive reals | willingness to accept smaller gaps (required gap ÷ this value) |
| lcSigma | 0.0 | — | lateral positioning imperfection |
| lcCooperativeHelpTime | 60 s | — | time threshold for yielding to blocked strategic changers |

### B6. SUMO junction / gap-acceptance model parameters

| Parameter | Default | Meaning | Source |
|---|---|---|---|
| jmCrossingGap | 10 m | min. distance to an approaching pedestrian before a vehicle may cross a crossing (excl. walking area) | `vehdef.md`, retrieved 2026‑09‑17 |
| jmIgnoreKeepClearTime | −1 (always try to keep junction clear) | time after which a vehicle may enter and block a junction | same |
| jmIgnoreFoeProb / jmIgnoreFoeSpeed | 0 / 0 | probabilistic right-of-way violation model | same |
| impatience | 0.0 (grows via `--time-to-impatience`, default 180 s) | driver's willingness to impede higher-priority traffic; grows with wait time | same |
| Pedestrian impatience `timeToMaxImpatience` | hard-coded 120 s | same concept applied to pedestrians crossing without priority | same |

### B7. Wiedemann car-following models (74/99)

**Wiedemann 99** (used in PTV Vissim) parameter set — source: PTV Vissim online help, "Defining the Wiedemann 99 model parameters," https://cgi.ptvgroup.com/vision-help/VISSIM_2023_ENG/Content/4_BasisdatenSim/FahrverhaltensparameterFolgeverh_Wied99.htm (accessed 2026‑09‑18):

| Param | Unit | Meaning | Default (where stated) |
|---|---|---|---|
| CC0 | m | Standstill distance | (default not given on fetched page — commonly cited as ~1.5 m in Vissim docs elsewhere; **UNVERIFIED here**) |
| CC1 | s | Headway time distribution | (not given on fetched page; commonly cited ~0.9 s elsewhere — **UNVERIFIED here**, see cross-check below) |
| CC2 | m | "Following variation" — extra distance beyond safety distance before a driver closes back in | **4.0 m** (explicitly confirmed) |
| CC3 | s | Time before entering "braking" reaction (negative) | not quantified on this page |
| CC4 / CC5 | m/s | Negative/positive speed-difference thresholds during following | not quantified on this page |
| CC6 | 1/(m·s) | Distance's influence on speed-oscillation | not quantified on this page |
| CC7 | m/s² | Oscillatory acceleration | not quantified on this page |
| CC8 | m/s² | Acceleration from standstill | not quantified on this page |
| CC9 | m/s² | Acceleration at 80 km/h | not quantified on this page |

Cross-check (secondary, via web search snippet, not independently opened): a Wisconsin DOT VISSIM calibration manual and a ResearchGate figure report **CC0 ≈ 4.92 ft (1.5 m), CC1 ≈ 0.90 s, CC2 ≈ 13.12 ft (4.0 m)** — consistent with the CC2 = 4.0 m value confirmed directly above. Source: WisDOT TEOpS 16-20 attachment 6.3, https://wisconsindot.gov/dtsdManuals/traffic-ops/manuals-and-standards/teops/16-20att6.3.pdf (surfaced via WebSearch 2026‑09‑18; **not independently opened/verified this session — flag as secondary/UNVERIFIED-direct**).

**Wiedemann 74**: **UNVERIFIED** — no cached source and no direct fetch performed this session; do not use invented numbers. If needed, the PTV Vissim help page family (`FahrverhaltensparameterFolgeverh_Wied74.htm`) is the logical next fetch target.

### B8. Gipps (1981) car-following model

Source: Wikipedia, "Gipps' model," https://en.wikipedia.org/wiki/Gipps%27_model (accessed 2026‑09‑18) — no typical numeric values given in this source (flagged below).

| Parameter | Symbol | Meaning |
|---|---|---|
| Max desired acceleration | aₙ | "Maximum acceleration which the driver of vehicle n wishes to undertake" |
| Most severe braking | bₙ (< 0) | "Most severe braking that the driver of vehicle n wishes to undertake" |
| Desired speed | Vₙ | driver n's desired travel speed |
| Effective vehicle size | sₙ | physical length + margin |
| Apparent reaction time | τ | constant across all vehicles |

Free-flow term: vₙ(t+τ) ≤ vₙ(t) + 2.5aₙτ(1 − vₙ(t)/Vₙ)(0.025 + vₙ(t)/Vₙ)^½. Congested term is the braking-safety bound involving bₙ, τ, the gap, and the estimated most-severe deceleration of the leader b̂. **Typical numeric values (e.g. a≈1.5–2.5 m/s², b≈−3 m/s², τ≈2/3 s, quoted in some secondary sources) could not be confirmed from an openable source this session — mark as UNVERIFIED rather than reuse unconfirmed figures.**

### B9. Gap acceptance — HCM critical gap / follow-up time (two-way stop-controlled intersections, passenger cars)

Source: PTV Vision (VISUM) online help reproducing HCM base values, https://cgi.ptvgroup.com/vision-help/VISUM_2025_ENG/Content/1_Benutzermodell%20IV/1_5_Vorfahrtsgeregelte%20Knoten.htm (accessed 2026‑09‑18). (Direct HCM/FHWA PDF fetches failed to parse as text this session; this secondary reproduction is used instead and is flagged accordingly.)

| Movement | Base critical gap t_cb (s), major flow <4 lanes | t_cb (s), major flow ≥4 lanes | Base follow-up time t_fb (s) |
|---|---|---|---|
| Major-street left turn | 4.1 | 4.1 | 2.2 |
| Minor-street right turn | 6.2 | 6.9 | 3.3 |
| Minor-street through | 6.5 | 6.5 | 4.0 |
| Minor-street left turn | 7.1 | 7.5 | 3.5 |

Cross-check ratio: TRB search summary independently states the follow-up-time/critical-gap ratio is "approximately 0.60" — roughly consistent with the table (e.g. 2.2/4.1≈0.54, 3.3/6.2≈0.53, 4.0/6.5≈0.62, 3.5/7.1≈0.49). Source: TRID/TRB record "A Further Investigation on Critical Gap and Follow-up Time," surfaced via WebSearch 2026‑09‑18 (not independently opened).

### B10. Signal timing defaults (cycle, yellow, all-red)

| Item | Value | Source |
|---|---|---|
| Yellow change interval — MUTCD guidance | "approximately 3 to 6 seconds, with longer intervals on higher-speed approaches"; 2003 MUTCD minimum = 3 s | FHWA Signal Timing Manual (2008), Ch. 5, https://ops.fhwa.dot.gov/publications/fhwahop08024/chapter5.htm (accessed 2026‑09‑18) |
| Yellow interval by approach speed (Table 5‑7, paraphrased) | ranges from 3.0 s to 5.4 s across 25–60 mph approach speeds | same |
| Yellow interval formula (ITE kinematic formula) | y = t + v/(2a + 2Gg); t = perception-reaction time (≈1 s), v = approach speed (ft/s), a = deceleration (≈10 ft/s²), g = 32.2 ft/s², G = grade (≈0) | FDOT training webinar (redlightrobber.com "Derivation of the Yellow Change Interval Formula" corroborates same formula), surfaced via WebSearch 2026‑09‑18; **direct FDOT/redlightrobber PDFs failed to parse as text this session — treat formula as secondary/well-established engineering formula, not independently re-verified by direct read** |
| All-red (red clearance) interval — MUTCD guidance | "should not exceed 6 seconds" | FHWA Signal Timing Manual Ch. 5, accessed 2026‑09‑18 |
| All-red interval — practitioner survey | practitioners' standard red-clearance values ranged **0.5–2.0 s**; "optional, no consensus on application/duration" | same |
| All-red/total clearance formula | TC = t + v/2a + (w+L)/v ; w = intersection crossing width, L = vehicle length (≈20 ft assumed) | surfaced via WebSearch 2026‑09‑18, secondary source, not independently re-verified |
| SUMO netconvert auto-generated TLS defaults — cycle time | **90 s** (`--tls.cycle.time`) | local cache `netconvert.md`, retrieved 2026‑09‑17 |
| SUMO netconvert — green phase default | 31 s (`--tls.green.time`) | same |
| SUMO netconvert — red phase (no conflicting flow) default | 5 s (`--tls.red.time`) | same |
| SUMO netconvert — intermediate all-red default | 0 s (`--tls.allred.time`), i.e. disabled unless requested | same |
| SUMO netconvert — yellow computed from kinematics | `--tls.yellow.time` default **-1** (auto-computed) using `--tls.yellow.min-decel` default **3 m/s²** | same |
| SUMO netconvert — pedestrian crossing green/clearance | min green **4 s** (`--tls.crossing-min.time`), clearance **5 s** (`--tls.crossing-clearance.time`) | same |
| SUMO netconvert — left-turn phase default duration | 6 s (`--tls.left-green.time`) | same |
| SUMO netconvert — variable-phase min/max duration defaults | min 5 s / max 50 s (`--tls.min-dur` / `--tls.max-dur`) | same |
| Cycle length — general urban practice | NACTO Urban Street Design Guide: short cycles of **60–90 s** are recommended for urban permeability/frequent ped crossings; cycles <60 s only w/ "feathering" near bottlenecks | NACTO, Signal Cycle Lengths, https://nacto.org/publication/urban-street-design-guide/intersection-design-elements/traffic-signals/signal-cycle-lengths/ — page returned HTTP 403 on direct fetch; content above is from the WebSearch result snippet only (accessed 2026‑09‑18), **flag as secondary/not independently opened** |
| Cycle length — FHWA initial-planning rule of thumb | 60 s for 2 critical phases, 75 s for 3 critical phases | same WebSearch summary, accessed 2026‑09‑18, secondary/not independently opened |

### B11. Social Force Model — pedestrians (Helbing & Molnár, 1995)

Source: Helbing & Molnár, "Social force model for pedestrian dynamics," Phys. Rev. E 51, 4282 (1995); arXiv:cond-mat/9805244. Local cache `sfm1995.pdf.txt`, retrieved 2026‑09‑17.

| Parameter | Value used in the paper's simulations |
|---|---|
| Desired speed distribution | Gaussian, mean ⟨v₀⟩ = **1.34 m/s**, std √θ = **0.26 m/s** (citing Henderson 1971/1974 empirical data) |
| Max acceptable speed | v_max = **1.3 × v₀** per pedestrian |
| Relaxation time τ | **0.5 s** |
| Repulsive potential (pedestrian–pedestrian), exponential form | V⁰_αβ = **2.1 m²/s²**, decay length σ = **0.3 m** |
| Repulsive potential (pedestrian–border) | U⁰_αB = **10 m²/s²**, decay length R = **0.2 m** |
| Step-width parameter Δt (elliptical potential geometry) | **2 s** |
| Field-of-view half-angle 2φ | **200°** |
| Behind-view influence weight c | **0.5** |
| Walkway width used in lane-formation demo | 10 m (lanes emerge above a critical density; N(W) ≈ 0.36 m⁻¹·W + 0.59 lanes) |
| Core equation form | d w⃗_α/dt = F⃗_α(t) + fluctuations; F⃗_α sums a desired-direction term F⃗⁰_α, pairwise pedestrian repulsion F⃗_αβ, border repulsion F⃗_αB, and optional attraction F⃗_αi |

### B12. Cyclist / other VRU speeds

| Mode | Speed | Source |
|---|---|---|
| Bicycle (SUMO default) | maxSpeed 50 km/h (physical), **desiredMaxSpeed 20 km/h** | `vtype_defaults.md`, retrieved 2026‑09‑17 |
| E‑scooter (SUMO default) | maxSpeed 25 km/h, desiredMaxSpeed 20 km/h | same |
| Moped | maxSpeed 45 km/h | same |
| Pedestrian (SUMO default) | maxSpeed 37.58 km/h (physical cap, ~world-record-derived), desiredMaxSpeed 5 km/h | same |
| Pedestrian desired speed (Helbing & Molnár empirical) | mean 1.34 m/s (≈4.8 km/h), σ=0.26 m/s | `sfm1995.pdf.txt` (see B11) |

### B13. SUMO pedestrian "striping" model parameters

Source: local cache `pedestrians.md`, retrieved 2026‑09‑17; canonical URL https://sumo.dlr.de/docs/Simulation/Pedestrians.html.

| Parameter | Default | Meaning |
|---|---|---|
| `--pedestrian.model` | striping | options: nonInteracting / striping / jupedsim |
| `--pedestrian.striping.stripe-width` | **0.65 m** | lateral stripe width for collision-avoidance lanes |
| `--pedestrian.striping.dawdling` | **0.2** | fraction of max speed randomly shaved off per step |
| `--pedestrian.striping.jamtime` | **300 s** | time stuck before entering 'jammed' override state (moves at ¼ max speed, ignoring obstacles, no collisions registered) |
| `--pedestrian.striping.jamtime.crossing` | **10 s** | shortened jam threshold while on a crossing |
| `--pedestrian.striping.jamtime.narrow` | **1 s** | shortened jam threshold on single-pedestrian-wide infrastructure |
| Oncoming-traffic reservation (junctions/crossings) | reserve **1/3** of width by default | `--pedestrian.striping.reserve-oncoming.junctions` |
| Oncoming-traffic reservation (normal lanes) | off by default, enable via `--pedestrian.striping.reserve-oncoming` | |
| Vehicle–pedestrian crossing blocking threshold | `jmCrossingGap` default **10 m** (see B6) | |

### B14. Weather effects on driving (FHWA)

Source: FHWA Road Weather Management Program, "How Do Weather Events Impact Roads?", https://ops.fhwa.dot.gov/weather/q1_roadimpact.htm (accessed 2026‑09‑18). **Note:** the task brief pointed at `fhwa_rab_ch4.pdf.txt` for this data, but that cached file is actually FHWA's *Roundabouts: An Informational Guide* Chapter 4 (Operation/Capacity) — it contains no weather content (verified by full read + grep). Roundabout capacity data extracted from it is filed under B17 instead; weather numbers below come from the correct FHWA source fetched fresh.

| Condition | Speed reduction | Capacity reduction |
|---|---|---|
| Freeway, light rain/snow | 3–13% | 4–11% |
| Freeway, heavy rain | 3–16% | 10–30% |
| Freeway, heavy snow | 5–40% | 12–27% |
| Freeway, low visibility (fog) | 10–12% | 12% |
| Arterial, wet pavement | 10–25% | — |
| Arterial, snowy/slushy pavement | 30–40% | — |
| Arterial traffic volume decrease | — | 15–30% |
| Arterial saturation flow rate reduction | — | 2–21% |

Other FHWA figures from the same page: weather-related crashes ≈ **12%** of all crashes/yr (~**745,000**/yr); of weather-related crashes, rain causes **77%**, freezing precipitation **18%**, low visibility **4%**, severe crosswind **1%**; arterial travel-time delay increase **11–50%**; non-recurring highway delay attributable to snow/ice/fog ≈ **23%**.

### B15. Fundamental diagram validation targets

| Quantity | Value | Source |
|---|---|---|
| Capacity drop at freeway bottleneck breakdown | typically **~20%** (order of magnitude) | Treiber/Hennecke/Helbing 2000, `idm2000.pdf.txt` |
| Capacity drop, realistic literature range | **5–20%** | Kerner & Rehborn (1996); Cassidy & Bertini (1999), cited in Kesting/Treiber/Helbing 2010, `iidm2010.txt` |
| Capacity drop, IDM+ACC simulation result (this paper) | **5–15%** | same |
| Jam (congested) density, IDM calibration | ρ_jam = **140 veh/km** | `idm2000.pdf.txt` |
| Jam propagation (stop-and-go) speed | ≈ **−15 km/h** | `idm2000.pdf.txt` (also independently observed in empirical German freeway data in the same paper) |
| Convective-stability flow threshold | Q_cv ≈ **1050 veh/h** | `idm2000.pdf.txt` |
| Theoretical max flow (triangular FD, IDM/Newell limit) | Q_max = (1/T)·(1 − l_eff/(v₀T + l_eff)) | Kesting/Treiber/Helbing 2010 Eq. 4.1, `iidm2010.txt` |
| Uncongested "constant-speed substream" validity range (empirical, freeway) | roughly **300–2200 pcphpl** (passenger cars per hour per lane) | Hall, "Traffic Stream Characteristics," Ch. 2 of FHWA Traffic Flow Theory monograph, local cache `tft_chap2.pdf.txt`, retrieved 2026‑09‑17 |
| Space-mean vs time-mean speed difference | Wardrop (1952): time-mean speed "6–12% greater than" space-mean speed in a mixed-speed (8–100 km/h) signalized-road dataset; difference "minimal" for uncongested freeway flow (Gerlough & Huber 1975; Drake et al. 1967 regression) | `tft_chap2.pdf.txt` |
| Roundabout single-lane approach: max circulating flow before double-lane needed | **1,800 veh/h** | FHWA *Roundabouts: An Informational Guide*, Ch. 4, local cache `fhwa_rab_ch4.pdf.txt` |
| Roundabout single-lane exit flow ceiling before double-lane exit needed | **1,200 veh/h** (practical range 1,200–1,300; theoretical ceiling ~1,400 veh/h) | same |
| Target roundabout/unsignalized degree-of-saturation design ceiling | **0.85** (Australian design guidance, adopted by FHWA guide) | same |

### B16. Vehicle class dimensions (recap)

See full table in **B4**. Additional non-motor-vehicle classes from `vtype_defaults.md` (retrieved 2026‑09‑17):

| vClass | length × width × height (m) | mass |
|---|---|---|
| tram | 22 × 2.4 × 3.2 | 37,900 kg |
| rail_urban | 3×36.5 × 3.0 × 3.6 | 59,000 kg |
| rail | 2×67.5 × 2.84 × 3.75 | 79,500 kg |
| rail_electric | 8×25 × 2.95 × 3.89 | 83,000 kg |
| rail_fast (ICE-class) | 8×25 × 2.95 × 3.89 | 409,000 kg |
| ship | 17 × 4 × 4 | 100,000 kg |

### B17. Roundabout capacity / operations (FHWA RAB Ch. 4)

Source: FHWA, *Roundabouts: An Informational Guide*, Chapter 4 "Operation," local cache `fhwa_rab_ch4.pdf.txt`, retrieved 2026‑09‑17.

| Item | Value |
|---|---|
| Passenger car equivalents (pce) | Car 1.0; single-unit truck/bus 1.5; truck+trailer 2.0; bicycle/motorcycle 0.5 |
| Recommended max degree of saturation | 0.85 (AU/DE/UK practice, adopted by this guide) |
| Little's Law queue estimate | L = v·d/3600 (L=veh, v=entry flow veh/h, d=avg delay s/veh) |
| HCM control-delay formula reference | Eq. 4‑7 (standard unsignalized-intersection delay formula, T=0.25 h for a 15-min period) |
| Short-lane capacity multiplier (double-lane approach, by # vehicle spaces n_f) | n_f=0→0.500; 1→0.707; 2→0.794; 4→0.871; 6→0.906; 8→0.926; 10→0.939 |
| International software tools referenced | ARCADY, RODEL (UK); SIDRA (Australia); HCS‑3 (US HCM 1997); KREISEL (Germany); GIRABASE (France) |

---

## TOPIC C — VeReMi / Extension / NextGen, F2MD, ETSI TR 103 460 / TS 103 759

### C1. VeReMi (original, 2018)

Source: van der Heijden, Lukaseder & Kargl, "VeReMi: A Dataset for Comparable Evaluation of Misbehavior Detection in VANETs," SecureComm 2018. Local cache `site_veremi.html.txt` + `veremi_index.md`, retrieved 2026‑09‑17/24.

| Item | Value |
|---|---|
| Simulator | VEINS (modified, based on v4.6) + LuST v2 (Luxembourg SUMO Traffic) |
| Message types in log | `type=2` (own GPS ground truth), `type=3` (received BSM via DSRC) |
| File naming | `JSONlog-<vehNum>-<omnetModuleId>-A<0/1>.json` (`A0`=genuine, `A1`=attacker) |
| Ground truth | Per-simulation ground-truth file + per-vehicle reception logs |
| Design axes | 3 density levels × 5 attack types × 3 attacker densities (0.1/0.2/0.3 from the index) |
| Index size (this session's cached index) | 241 result rows shown (repetition × density × attackerType × attackerDensity), attacker types enumerated as integers 1,2,4,8,16 (bitmask-style attack IDs) |
| Distribution | Code: GitHub (VeReMi-dataset org, `securecomm2018` branch); Dataset: Zenodo |

### C2. VeReMi Extension (2020)

Source: Kamel, Wolf, van der Heijden, Kaiser, Urien & Kargl, "VeReMi Extension," ICC 2020. Local cache `site_veremi-extension.html.txt`, retrieved 2026‑09‑17/24.

| Item | Value |
|---|---|
| Generator | F2MD (extension of VEINS), OMNeT++ 5.6.1, SUMO 1.5.0 |
| Scenario | LuST subsection, ~1.61 km² |
| Time periods | 07:00–09:00 (rush hour, high density); 14:00–16:00 (low density); plus `MixAll_0024` (00:00–24:00 mixed test-bench) |
| Attacker penetration | **30%** in all simulations |
| Attack categories → types | Time-related: Delayed Messages. Position: ConstPos, RandomPos, ConstPosOffset, RandomPosOffset. Speed: ConstSpeed, RandomSpeed, ConstSpeedOffset, RandomSpeedOffset. Network: DoS, DoSRandom. Replay: DataReplay, Disruptive. Identity (Sybil): TrafficCongestionSybil, DoSRandomSybil, DataReplaySybil, DoSDisruptiveSybil. Multi-param: EventualStop |
| vs. original VeReMi | Adds multi-attribute attacks + sensor error models (both absent in original) |

### C3. VeReMi NextGen (2026)

Source: Hermann, Remmers, Eisermann, Erb & Kargl, "VeReMi NextGen: A Dataset for Evaluating Misbehavior Detection Systems in VANETs," VNC 2026. Local cache `site_veremi-nextgen.html.txt`, retrieved 2026‑09‑17/24.

| Item | Value |
|---|---|
| Simulator stack | MOSAIC v25.0 + SUMO v1.22.0 + OMNeT++ v6.1, using the **InTAS** scenario (Ingolstadt, replacing LuST) |
| Scenario area | 10.2 km² total (separate train/val vs. test geographic areas, no spatial overlap) |
| Scenario types (4) | Urban 2 AM (low density), Urban 7 AM (high density), Highway 2 AM (low density), Highway 7 AM (high density) |
| Dataset size | 180 subsets = 15 attack subsets × 4 scenarios × 3 splits (train/val/test) |
| Durations | Urban/Highway 2 AM: 9000 s train / 1800 s val / 7200 s test. Urban/Highway 7 AM: 375 s / 75 s / 300 s |
| File format | One JSON file per receiving vehicle per subset, containing all messages it received |
| Attacker density | **20%** of vehicles |
| Label semantics | Each message has an `attacker` field: 1 = significant deviation in ≥1 attribute, 0 = legitimate/below-significance-threshold |
| Attack categories → 15 types | Time: Time Delay Attack. Position: Constant Position Offset, Random Position Offset, Position Mirroring. Speed: Constant Speed Offset, Random Speed Offset, Zero Speed Report, Sudden Constant Speed. Heading: Reversed Heading. Acceleration: Feigned Braking, Acceleration Multiplication. Multi-parameter: Sudden Stop, DoS Attack, Traffic Congestion Sybil, Data Replay |
| New vs. Extension | 6 newly designed attacks (position mirroring, zero speed report, reversed heading, feigned braking, acceleration multiplication, + one more multi-param); adds heading/acceleration attack surface; adds 3 driver profiles (normal/cautious/aggressive); adds fixed train/val/test splits; built for future VRU (pedestrian/cyclist) extension |
| Pipeline | 2-stage: (1) MOSAIC+InTAS simulation → "Baseline" dataset of received CAMs; (2) post-processing injects attacks into the Baseline without rerunning the simulation |

### C4. F2MD detector list + thresholds (from the actual cloned `veins-f2md` source, not the top-level python repo)

**Important caveat:** the pre-cloned `f2md/` directory in the scratchpad has `veins-f2md`, `inet`, and `simulte-f2md` as **empty git-submodule placeholders** (never checked out) — only the Python attack-server/misbehavior-authority-server/ML-server scaffolding was present. To satisfy the task's explicit instruction to "grep its source for detector thresholds," I cloned the actual detector source fresh this session: `git clone --depth 1 https://github.com/josephkamel/veins-f2md.git` (accessed 2026‑09‑18). All values below are read directly from that source.

**19 detector checks** (`src/veins/modules/application/f2md/mdEnumTypes/MdChecksTypes.h`):
ProximityPlausibility, RangePlausibility, PositionPlausibility, SpeedPlausibility, PositionConsistancy, PositionSpeedConsistancy, PositionSpeedMaxConsistancy, SpeedConsistancy, BeaconFrequency, Intersection, SuddenAppearence, PositionHeadingConsistancy, kalmanPSCP, kalmanPSCS, kalmanPSCSP, kalmanPSCSS, kalmanPCC, kalmanPACS, kalmanSCC.

**Misbehavior classes** (`mdEnumTypes/MbTypes.h`): `Genuine`, `LocalAttacker`, `GlobalAttacker`.

**Detection thresholds** (`F2MDParameters.h`, "Detection Parameters" block):

| Constant | Value | Used by (per `ExperiChecks.cc` grep) |
|---|---|---|
| MAX_PROXIMITY_RANGE_L | 30 m | ProximityPlausibilityCheck (longitudinal box) |
| MAX_PROXIMITY_RANGE_W | 3 m | ProximityPlausibilityCheck (lateral box) |
| MAX_PROXIMITY_DISTANCE | 2 m | ProximityPlausibilityCheck |
| MAX_CONFIDENCE_RANGE | 10 | general confidence bound |
| MAX_PLAUSIBLE_RANGE | 420 m | RangePlausibilityCheck (communication range plausibility) |
| MAX_TIME_DELTA | 3.1 s | PositionSpeedMaxConsistancyCheck / PositionSpeedConsistancyCheck window |
| MAX_DELTA_INTER | 2.0 s | IntersectionCheck time window |
| MAX_SA_RANGE | 420 m | SuddenAppearenceCheck |
| MAX_SA_TIME | 2.1 s | SuddenAppearenceCheck (via `getLatestBSMAddr` time gate) |
| MAX_KALMAN_TIME | 3.1 s | all Kalman-filter consistency checks |
| KALMAN_POS_RANGE | 1.0 | Kalman position confidence |
| KALMAN_SPEED_RANGE | 4.0 | Kalman speed confidence |
| KALMAN_MIN_POS_RANGE | 4.0 | floor on Kalman position confidence |
| KALMAN_MIN_SPEED_RANGE | 1.0 | floor on Kalman speed confidence |
| MIN_MAX_SPEED | 40 m/s | fallback plausible max speed (→`MAX_PLAUSIBLE_SPEED` via `myLimits.x`, `omnetpp.ini` `MIN_MAX_SPEED`) |
| MIN_MAX_ACCEL | 3 m/s² | fallback plausible max accel (→`MAX_PLAUSIBLE_ACCEL` via `myLimits.y`) |
| MIN_MAX_DECEL | 4.5 m/s² | fallback plausible max decel (→`MAX_PLAUSIBLE_DECEL` via `myLimits.z`) |
| MAX_MGT_RNG | 4 | PositionSpeedMaxConsistancyCheck margin |
| MAX_MGT_RNG_DOWN | 6.2 | PositionSpeedConsistancyCheck margin (speed-dependent quadratic add-on) |
| MAX_MGT_RNG_UP | 2.1 | PositionSpeedConsistancyCheck margin |
| MAX_BEACON_FREQUENCY | 0.9 s | BeaconFrequencyCheck min inter-beacon time |
| MAX_DISTANCE_FROM_ROUTE | 2 m | PositionPlausibilityCheck (map-matching tolerance) |
| MAX_NON_ROUTE_SPEED | −1 | PositionPlausibilityCheck (speed floor for applying route check) |
| MAX_HEADING_CHANGE | 90° | PositionHeadingConsistancyCheck |
| DELTA_BSM_TIME | 5 s | BSM storage window |
| DELTA_REPORT_TIME | 5 s | report storage window |
| POS_HEADING_TIME | 1.1 s | position/heading consistency time gate |
| MAX_TARGET_TIME / MAX_ACCUSED_TIME | 2 s / 2 s | storage retention |

**Attack parameters** (same file): `LOCAL_ATTACKER_PROB=0.05`; mixed local-attack list of 19 named attack types (ConstPos, Disruptive, RandomPos, StaleMessages, DoSRandomSybil, ConstPosOffset, ConstSpeed, DoS, RandomPosOffset, DataReplaySybil, DoSDisruptive, ConstSpeedOffset, RandomSpeedOffset, EventualStop, DoSDisruptiveSybil, DataReplay, DoSRandom, GridSybil, RandomSpeed); `GLOBAL_ATTACKER_PROB=0.0` (default off), `GLOBAL_ATTACK_TYPE=MAStress`; attack-magnitude constants `RandomPosOffsetX/Y=70.0`, `RandomSpeedX/Y=40.0`, `RandomSpeedOffsetX/Y=7.0`, `RandomAccelX/Y=2.0`, `StopProb=0.05`, `StaleMessages_Buffer=60`, `DosMultipleFreq=4`, `ReplaySeqNum=6`, `SybilVehNumber=5`, `SybilDistanceX/Y=5/2`.

**Pseudonym-change parameters**: `Period_Change_Time=240 s`, `Tolerance_Buffer=10`, `Period_Change_Distance=80 m`, `Random_Change_Chance=0.1`.

Source for this entire block: `veins-f2md` GitHub repo (Joseph Kamel), files `src/veins/modules/application/f2md/F2MDParameters.h`, `mdEnumTypes/MbTypes.h`, `mdEnumTypes/MdChecksTypes.h`, `mdChecks/ExperiChecks.cc` — cloned and grepped directly, accessed 2026‑09‑18.

### C5. ETSI TS 103 759 V2.2.1 (2026‑01) — Misbehaviour Reporting service, message structure

Source: local cache `etsi103759.pdf.txt`, retrieved 2026‑09‑17.

| Item | Value |
|---|---|
| Scope | Specifies the Misbehaviour Reporting (MR) service: local ITS-S detections → reports sent to a central Misbehaviour Authority (MA) |
| MDM system components | ITS-S Local Misbehaviour Detection (probe + MR generation/transmission), optional Misbehaviour Pre-processing, Misbehaviour Authority, Remediation |
| Local MDS role | Runs individual + combined detectors on incoming messages (may fuse sensor input); decides if the result is "suitable for reporting" |
| MR generation role | Decides whether to generate an MR, assembles/signs/encrypts it, sends or stores it, manages storage prioritization over time |
| MR report top-level structure (§7) | Hierarchical: `EtsiTs103759Mbr` module wraps ITS-AID-specific report content; ASN.1 defined in Annex A across modules: Misbehaviour Report, MR data structures, App-agnostic-reporting, CAM-reporting, DENM-reporting, Base types, Common observations, BSM-reporting |
| Reportable content restriction | "only observations which are directly attributable to specific ITS services and related messages may be the object of a misbehaviour report" |
| Certificate profiles covered (§8) | ITS-S signing certificate (with SSP), Misbehaviour Authority certificate (with SSP), MRS/MDM certificate permissions |
| Informative annexes | Annex B: MDM system detailed view; Annex C: Local MD Service detailed view; Annex D: example individual detectors for CAMs and DENMs (incl. a DENM local-MD-strategy taxonomy) |
| Key terms defined | "misbehaviour" = transmitting false/misleading/unauthorized info (purposeful or not); "misbehaving entity" = ITS-S sending false/misleading messages *using valid certificates* (covers both faulty and malicious); "evidence" = information used to determine correctness of a statement |
| Normative references anchoring the security architecture | ETSI TS 102 940 (security architecture), TS 102 941 (trust/privacy mgmt), TS 103 097 (cert/security header formats), IEEE 1609.2‑2022 |

### C6. ETSI TR 103 460 V2.1.1 (2020‑10) — Misbehaviour Detection pre-standardization study, detection categories

Source: local cache `etsi103460.txt`/`tr103460.txt`, retrieved 2026‑09‑17.

| Item | Value |
|---|---|
| Scope | Overview of MD mechanisms suitable for C-ITS; comments on performance/applicability; hints toward minimum security-architecture requirements and MR distribution mechanisms |
| High-level detection-approach categories (§5.1) | False beacon information detection; False warning detection; Node trust evaluation; Feasibility assessment |
| Reporting approach categories (§5.2) | Unicast MR to the Misbehaviour Authority; Broadcast MR to neighbours (with pros/cons/alternatives discussed) |
| Use cases enumerated (§6) | UC1: plausibility checks on access-layer measurements of periodic CAMs; UC2: plausibility checks on periodic CAM content; UC3: security-level local checks on received C-ITS messages; UC4: misbehaviour detection on DENM traffic-event signalling |
| Architecture (§7) | General MD/MR architecture + Misbehaviour Report message format |
| Standard recommendations | §8 gives MD standardization recommendations (categories only surfaced in TOC; not re-extracted verbatim this session) |
| Annexes | Annex A: potential MD mechanisms for CAMs; Annex B: example ASN.1 MR spec; Annex C: MD with Collective Perception Messages (CPM) — general overview, CPM attack model, MD-with-CPM approaches, open issues list |
| Key abbreviation list includes | ART/eART (Acceptance Range Threshold / enhanced), CoE (Certainty of Event), MDM, MPP (Map-Proofed Position), SAW (Sudden Appearance Warning), LEAVE (Local Eviction of Attackers by Voting Evaluators), P2DAP (Privacy-Preserving Detection of Abuses of Pseudonyms) |
| Relationship to TS 103 759 | TR 103 460 is the informative pre-standardization analysis; TS 103 759 (C5 above) is the resulting normative MR service spec that cites TR 103 460 as background [i.3] |

---

## Summary of UNVERIFIED items (do not treat as fact without further work)

1. Whether an OSM-derived lane-geometry export counts as ODbL "Produced Work" vs. "Derivative Database" — OSM Foundation's own guideline states this is unresolved.
2. Overture Maps building-height attribute presence — not confirmed from the fetched landing page (schema reference not opened).
3. OSM building tag coverage/completeness percentage, and standard level→metre conversion factor — not found on the fetched wiki page.
4. SUMO's own project licence (stated as EPL-2.0 from general knowledge, not re-confirmed via a fetched licence file this session).
5. osm2streets' software licence (not stated in the cached README excerpt).
6. SRTM vertical accuracy figure (page deferred to an external paper not fetched).
7. Wiedemann 99 CC0/CC1/CC3–CC9 exact default values (only CC2=4.0 m confirmed on the primary PTV page fetched; CC0/CC1 given only via an unopened secondary WisDOT PDF surfaced by search).
8. Wiedemann 74 parameters — no source consulted at all this session.
9. Gipps (1981) typical numeric parameter values (a, b, τ, V) — model structure confirmed, but no numeric defaults confirmed from an openable source.
10. ITE yellow-change / all-red kinematic formulas — formulas stated consistently across search snippets, but the primary FDOT/redlightrobber PDFs failed to parse as text this session, so treat as secondary-sourced.
11. NACTO cycle-length guidance (60–90 s) and FHWA "60/75 s initial planning" rule — obtained only via WebSearch summary; NACTO page itself returned HTTP 403 on direct fetch.
12. ETSI TR 103 460 §8 "standard recommendations" content — only the table-of-contents heading was captured, not the substantive text.
