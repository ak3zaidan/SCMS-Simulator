# World crate defect register (adversarial review, 2026-09-18)

Two independent reviewers audited `v2xw-world` after it imported real
Midtown Manhattan: a code-and-robustness reviewer and a visual/statistical
reviewer. Their findings are reproduced verbatim below. Both confirmed the
geometry and projection are correct; the defects are attribute, scope and
edge-case defects, plus one real geometric bug.

## What both reviewers proved correct

```
PASSED, on the real 30 MB Phase 1 extract rather than a fixture. `./target/release/examples/import_osm worlds/cache/manhattan.osm.xml <dir> 2026-09-18T00:00:00Z` was run twice, in two separate processes, into two separate directories, and all four artefacts are byte-identical: world.vwb cea2ed9bc39393f1b2197a574ff54571e9118c8039e6647025e35188316a68bb, world.json f8c28cde9a972108c9dcf77432275fb703fcb18198600ad4ca7f078d945b82ce, world.v2xw 34b2ccc51246e361a3cb83f1b34a4fc5b6a6ffa1a99550a8f9dcbcff8cfa3fe2, report.txt 9bc02ec5cb50ee938aaf4f024544e2624cc8a10bf1720951ee2dc4ab92191f00. Byte identity of world.v2xw and world.vwb implies identical id assignment (both are dense-id-ordered tables), identical ordering and an identical content hash (987742cc66446cde60ce31dee60444f69199e0f644e79c043e76ef71463f8bbe). The mechanisms behind it check out: `grep -rn "HashMap\|HashSet"` over src/, tests/ and examples/ returns only the four doc comments that claim there are none; `grep -rnE "\.(sin|cos|tan|atan2|exp|ln|pow[fi]|sqrt|hypot|...)\("` finds no std transcendental outside `v2xw_core::math` (the o
```

```
Strong. I compared the serialiser field by field against the §4.1/§4.2/§4.3/§4.4/§4.5/§4.6 and §2.5 tables and found no layout disagreement: the 16-byte header, the 192-byte directory (every one of the 31 fields at the documented offset), the eleven struct-of-array lane columns at 0/4L/8L/12L/16L/20L/24L/28L/32L/34L/35L, the nine building columns at 0/4B/8B/12B/16B/20B/24B/25B/26B, the two-array shared ring block, the 24/28/32/28/16-byte junction/signal/site/crossing/landuse records with their reserved bytes zeroed, and the StrTable's `n`, `blob_bytes`, `n+1` offsets and 4-padded blob. Then I tested it against the independently written TypeScript client rather than reading it. Using ui/packages/protocol/dist under Node 24: `decodeWorld` parses both the 2 021 428-byte Manhattan payload and the 24 736-byte procedural payload with every section decoding, `computeWorldContentHashWith` reprod
```

## Code and robustness review — 15 findings

### R1 [HIGH] `/Users/ahmedzaidan/Developer/SCMS-Simulator/crates/v2xw-world/src/osm.rs`:2218

**Problem.** offset_polyline's mitre cap (MITRE_MIN_COS = 0.35, line 2220) moves each vertex along the mitre without pruning inverted vertices, so offsetting a bend whose radius is smaller than the lane offset inverts that part of the lane and the resulting centreline crosses itself. Nothing detects it: Lane::new accepts it, World::validate accepts it, and there is no Anomaly category for it (30 categories, none covers a self-intersecting polyline). MEASURED on the Phase 1 Manhattan extract (worlds/cache/manhattan.osm.xml, 13 760 lanes): 56 lanes have self-intersecting centrelines - 8 LaneKind::Driving and 48 LaneKind::Sidewalk. The consequence is that arc length is no longer injective in space: on lane l7790 the crossing point (641.75, 1013.38) has arc length 15.24 m AND 24.76 m, 9.52 m apart, and Lane::project_point returns only s = 24.76 (d = 0.0). Every mobility model that computes a gap, a leader or a lateral offset from (lane, s) will therefore place a vehicle up to 9.5 m from where it is. The source polylines do not self-intersect (sampled 4 long `service` ways) - offsetting creates it.

**Proposed fix.** Prune the offset polyline after offsetting: drop vertices whose mitre shift inverts the local direction of travel (dot(out[i+1]-out[i], points[i+1]-points[i]) < 0), or cap |d_m| at the local radius of curvature, which is the standard offset-polyline pruning pass. Then add a `SelfIntersectingLane` (or reuse `DegenerateLane`) anomaly so an import that still produces one is counted rather than silent, and assert the absence of self-intersections in the Manhattan acceptance test.

### R2 [MEDIUM] `/Users/ahmedzaidan/Developer/SCMS-Simulator/crates/v2xw-world/src/osm.rs`:1613

**Problem.** classify_way applies the `maxspeed` tag to every WayFamily, so a vehicle speed limit becomes a footway's or a cycleway's walking speed. The HIGHWAY_TABLE gives footway/pedestrian/path 1.39 m/s and steps 0.5 m/s precisely because "a footway's speed limit is a walking speed", but any `maxspeed` tag overrides it and no anomaly is counted. MEASURED on Manhattan: 7 LaneKind::Sidewalk lanes carry a walking speed above 2.5 m/s, the worst being 11.176 m/s (40 km/h). Synthetic proof: `highway=footway` + `maxspeed=25 mph` yields two Sidewalk lanes at 11.176 m/s with 0 anomalies; `highway=steps` + `maxspeed=60` yields Sidewalk lanes at 16.667 m/s (60 km/h up a staircase); `highway=cycleway` + `maxspeed=60` yields Cycle lanes at 16.667 m/s.

**Proposed fix.** Read `maxspeed` only for WayFamily::Motor. For Foot and Cycle keep the table default (or accept `maxspeed` only when it is below a family ceiling, e.g. 2.5 m/s for Foot and 8 m/s for Cycle) and count an anomaly when a tag is ignored, so the override is visible in the report.

### R3 [MEDIUM] `/Users/ahmedzaidan/Developer/SCMS-Simulator/crates/v2xw-world/src/osm.rs`:2975

**Problem.** When an approach lane's end and a departure lane's start are closer than MIN_LANE_LENGTH_M (1 m), add_movement emits a `direct` Connection with `permitted: true` and returns before pushing anything onto `movements`. apply_restrictions (line 3106) walks only `net.movements[j]`, and conflict_matrix is built only from `net.movements[j]`, so such a movement is invisible to both: a turn restriction covering it is never applied and it gets no conflict-matrix row. PROVED with a 4-node fixture (three primary/residential ways meeting at a short junction): a `type=restriction` / `restriction=no_straight_on` relation with correct from/via/to members yields restrictions_applied = 0, 4 connector-less straight-on connections all still `permitted = true`, and only an `Anomaly::RestrictionUnmatched` count - which conflates "the via node is not a junction of this world" with "the junction exists but the movement is not in our movement list". On Manhattan this affects 1 movement, and 48 restrictions are reported unmatched against 34 applied.

**Proposed fix.** Record the direct case in `movements` as well, with an `internal: Option<LaneId>` set to None, so apply_restrictions and conflict_matrix see every movement; emit the Connection from the same loop that emits the connector movements. Failing that, run apply_restrictions over the `direct` vector too, and give the "junction found but movement absent" case its own Anomaly so it is distinguishable from a genuinely unmatched relation.

### R4 [MEDIUM] `/Users/ahmedzaidan/Developer/SCMS-Simulator/crates/v2xw-world/src/osm.rs`:3163

**Problem.** apply_restrictions matches a restriction's `from`/`to` members by OSM way id alone (the `names` closure tests `net.edge_info[edge].ways.contains(w)`), and parse_restriction (line 3085) throws away which turn the restriction names - every `no_*` collapses to RestrictionKind::Forbid. When the `from` way passes through the via node (common, because OSM does not require the from way to be split there) the importer's own splitting produces two approach edges that both carry that way id, so a single directional restriction bans the movement on both. PROVED: one `restriction=no_left_turn` from way 10 (nodes 1-2-3, via node 2) to way 12 banned four connection records across TWO distinct approach edges - `l1 (Main) -> l8 (West) dir=Left` (correct) and `l6 (Main) -> l8 (West) dir=Right` (a legal right turn from the opposite approach). This removes legal movements from the routable graph. It does not fire on the Manhattan extract, where 34 applied restrictions touch exactly 34 distinct approach edges.

**Proposed fix.** Keep the turn direction parsed from `restriction=no_left_turn` / `only_straight_on` in RestrictionKind and require `turn_matches(tagged, movement.turn)` in addition to the way-id match; additionally require the `from` edge to be the one whose last contributing way is the `from` way, so an approach that merely shares a way id is not matched.

### R5 [MEDIUM] `/Users/ahmedzaidan/Developer/SCMS-Simulator/crates/v2xw-world/src/quant.rs`:108

**Problem.** quant::is_on_grid returns true for every non-finite value, so the D9 scan in World::validate (model.rs:3498) cannot see a NaN or an infinity, and validate has no finiteness check outside Lane::new and Building::new's footprint loop. A non-finite float therefore reaches both writers - and they disagree about it. PROVED: a Building with height_m = f64::NAN passes World::builder().build(); the vwp binary stores NaN in the buildings height_m column while serde_vwp::to_json_with_hash's `f()` (serde_vwp.rs:642) turns it into JSON `null`. A height of 1e306 gives binary +inf and JSON `null`. vwp-v1 §0 makes NaN the float "absent" sentinel and §4.6 requires the JSON form to be "a direct transcription" of the binary, so the two forms are not interchangeable, and the TypeScript client's `WorldJsonBuilding.height_m: number` becomes `null`.

**Proposed fix.** Make World::validate reject a non-finite float in any field the spec does not declare as a sentinel (use `value.is_finite()` in the scan_exported_floats visitor rather than is_on_grid), and decide one rule for the JSON form - either it may not contain a non-finite value at all, or §4.6 gains an explicit encoding for the NaN sentinel (e.g. the JSON string "NaN") that both implementations use.

### R6 [MEDIUM] `/Users/ahmedzaidan/Developer/SCMS-Simulator/crates/v2xw-world/src/quant.rs`:96

**Problem.** quant::quantise is a private second copy of v2xw_core::math::quantize_to, and it behaves differently: it has no guard for the scale multiplication overflowing, so `(value * inv).round() / inv` returns +-inf for large magnitudes, where core returns the value unchanged. D9 names one central writer-side encoder (`v2xw_core::math::grid_index`), and core's own documentation says the helper "is the helper v2xw-world had to invent as crate::quant::grid_index; it belongs here, with the quantiser whose rounding it must match, so that the two cannot drift" - yet the duplicate is still here. MEASURED divergence: 13 of 52 sampled (value, quantum) pairs disagree, e.g. quantise(1e306, 1e-3) = inf vs quantize_to = 1e306, and quantise(1.7e305, 1e-6) = inf vs 1.7e305. Consequence: Building::new(height_m = 1e306) stores height_m = inf. Two writers in different crates quantising the same field can therefore produce different bytes, which is exactly the bug class D9 exists to close.

**Proposed fix.** Delete quant::quantise / quant::is_on_grid / quant::grid_index and re-export v2xw_core::math::quantize_to / is_on_grid / grid_index under the existing names, keeping the Q_* constants and quantise_f32 in this module. If the duplicate must stay for the non-panicking quantum contract, copy core's `if !scaled.is_finite() { return x; }` guard verbatim and add a test that the two agree over the sampled range.

### R7 [MEDIUM] `/Users/ahmedzaidan/Developer/SCMS-Simulator/ui/packages/protocol/src/world.ts`:121

**Problem.** LANDUSE_CLASSES is `["urban","suburban","rural","highway","park","water","industrial"]`, so the TypeScript client decodes class code 4 as "park" and 5 as "water". docs/protocol/vwp-v1.md §4.5 is normative and says `4` water, `5` park; the crate follows the specification (LanduseClass::Water = 4, Park = 5, model.rs:2425-2436) and already documents the mismatch at model.rs:2400-2410. This is the ONLY field-level disagreement I found between the two implementations. MEASURED: decoding the crate's real 2 021 428-byte Manhattan payload with ui/packages/protocol/dist and comparing worldToJson against the crate's world.json field by field gives 308 disagreements out of 337 land-use zones and zero disagreements anywhere else (13 760 lanes, 50 102 lane points, 7 390 buildings, 67 429 ring points, 3 421 junctions, 1 514 signal heads, 963 crossings, 819 strings, the provenance and the content hash all identical). Central Park renders as open water in the viewer.

**Proposed fix.** Reorder the TypeScript table to `["urban","suburban","rural","highway","water","park","industrial"]` and add a test asserting each name against the §4.5 code it is documented with, so the table cannot drift from the spec again.

### R8 [MEDIUM] `/Users/ahmedzaidan/Developer/SCMS-Simulator/crates/v2xw-world/src/osm.rs`:886

**Problem.** parse_osm sets `check_end_names = false` and never requires an `<osm>` root, and import_parsed reports nothing when there is nothing to import, so a document that is not an OSM extract at all becomes a valid, empty, content-addressed world rather than an error. PROVED: an Overpass failure document (`<osm version="0.6" generator="Overpass API"><remark>runtime error: Query timed out ...</remark></osm>`) imports to a world with 0 lanes, 0 nodes, 0 ways, **0 anomalies**, a bbox of (0,0,0)-(0,0,0), a provenance record and a content hash, and World::validate returns Ok. An HTML `502 Bad Gateway` page does the same. A failed download is therefore indistinguishable from a legitimately empty bounding box, and the resulting empty world will be cached under its hash. (A binary .osm.pbf is correctly rejected, with a UTF-8 error, and truncated XML is correctly rejected with WorldError::Malformed.)

**Proposed fix.** Require an `<osm>` root element in parse_osm and return WorldError::Malformed otherwise; and in import_parsed, when the file yields no nodes at all, either return an error or record a new `Anomaly::EmptyDocument` so the report cannot be silently clean.

### R9 [LOW] `/Users/ahmedzaidan/Developer/SCMS-Simulator/crates/v2xw-world/src/model.rs`:837

**Problem.** normalise_angle loops `while a > PI { a -= TAU }`, which does not terminate for a large finite input: at a = f64::MAX the subtraction is absorbed and the loop never exits, and at a = 1e9 it needs more than 1e7 iterations (measured with an iteration-capped copy of the loop: 1e9, 1e17, 1e300 and f64::MAX all exceed a 1e7 cap). It is reachable from outside the crate through the public `TurnDirection::from_heading_change` and `model::normalise_angle`, both of which document no bound on their argument. Every internal caller passes a difference of two `heading_2d()` results and is safe, so this is a public-API hazard rather than a live bug.

**Proposed fix.** Replace the loops with the arithmetic form `let a = a - TAU * (a / TAU).round(); if a <= -PI { a + TAU } else { a }`, which is multiplication, division and round only - still IEEE-exact and platform-independent - and terminates for every finite input. Keep the NaN pass-through.

### R10 [LOW] `/Users/ahmedzaidan/Developer/SCMS-Simulator/crates/v2xw-world/src/model.rs`:2051

**Problem.** normalise_ring decides a ring's winding from `ring_signed_area_2x`, which is exactly 0 for a self-intersecting (figure-of-eight) ring, so `(area < 0.0) == counter_clockwise` is false and the ring is left in whatever winding the source gave it - while docs/protocol/vwp-v1.md §4.4 declares payload rings counter-clockwise and Building::footprint documents the same. No Anomaly category covers a self-intersecting ring. PROVED: a bow-tie footprint (nodes 1-2-3-4-1 with crossing diagonals) imports as one building with 2A = 0, `point_in_ring(centre) == false` (so the obstacle contains no points), height 20 m, and 0 anomalies.

**Proposed fix.** Detect a self-intersecting ring in Building::new / LanduseZone::new (a segment-pair test over the ring, the primitive already exists as index::segments_intersect), add an `Anomaly::SelfIntersectingRing`, and either drop the ring or take its convex hull, recording the choice in the provenance.

### R11 [LOW] `/Users/ahmedzaidan/Developer/SCMS-Simulator/crates/v2xw-world/src/osm.rs`:4354

**Problem.** build_junction_shapes hulls the junction position together with the lane ends, then WorldBuilder::build quantises the position and the shape independently (RoadNetwork::quantise_in_place, model.rs:3898). Quantising a hull vertex and the position in opposite directions can leave the position outside its own polygon, so the implied property "the hull includes the junction's own node" does not survive quantisation. MEASURED on Manhattan (3 421 junctions): 2 positions are strictly outside their shape, worst depth 2.43e-4 m, and 715 more sit exactly on the boundary, which `point_in_ring`'s crossing-number convention reports as outside - 717 junctions in total for which `point_in_ring(j.shape, j.position)` is false.

**Proposed fix.** Quantise the hull's input points before calling convex_hull (quantise_vec3 on the lane ends and the position), or move build_junction_shapes after quantise_in_place, so the hull is computed on the values that are actually stored. Either way the property becomes exact rather than within-a-quantum.

### R12 [LOW] `/Users/ahmedzaidan/Developer/SCMS-Simulator/crates/v2xw-world/src/model.rs`:3556

**Problem.** D9 requires "a scanning test [that] fails the build if any output value sits off its grid", and scan_exported_floats is thorough for the geometry - but it does not visit WorldProvenance::transformations, whose parameters are f64 values serialised verbatim into every artefact this crate writes. PROVED: a procedural grid with block_x_m = 120.000_000_123_456_7 and building_height_m = 20.000_000_9 passes World::validate with an empty offender list, and the raw double `120.0000001234567` appears verbatim in serde_native::to_json, serde_native::to_bytes, serde_vwp::to_json_string AND the .vwb provenance blob. The blanket sweep that would have caught it (tests/world.rs:902, d9_no_native_json_number_is_a_raw_double) runs only on the two fixture worlds, whose parameters happen to be on-grid, and checks the 1e-7 grid rather than each field's own quantum, so a value on the degree grid but off the metre grid passes it.

**Proposed fix.** Quantise in Transformation::with (a float parameter is put on the quantum its name implies, or on Q_POSITION_M by default) or visit `provenance.transformations` from scan_exported_floats; and run the blanket JSON sweep over a world built with deliberately off-grid parameters so the test has a failing case to catch.

### R13 [LOW] `/Users/ahmedzaidan/Developer/SCMS-Simulator/crates/v2xw-world/src/hash.rs`:4

**Problem.** The crate contradicts itself about which of its two digests is the protocol's `Hello.world_hash`. hash.rs:4 says the geometry digest "is what the run manifest records, what the UI's Hello.world_hash carries and what the GET /world/{hash}.vwb URL is keyed by"; serde_vwp.rs:126 says the payload digest is "the {hash} of GET /world/{hash}.vwb and of Hello.world_hash". They are different numbers: for GridParams::legacy() the geometry digest is f1f30b38...4179 and the payload digest is dca41caa...644a, and WorldPayload::url_path() uses the latter. vwp-v1 §4.2 ("content_hash - SHA-256 of the body; MUST equal the URL hash and Hello.world_hash") makes the payload digest normative, so hash.rs is the stale one. A server that wires up the wrong one produces a Hello whose world_hash does not resolve to a payload.

**Proposed fix.** Correct the hash.rs module header: the geometry digest identifies the world and goes in the run manifest and the provenance; the payload digest is what §4.2 and §3.1.6 key the URL and Hello.world_hash by. Add a test asserting `payload.url_path()` contains `payload.content_hash_hex()` and not `content_hash_hex(&world)`, so the two cannot be confused again.

### R14 [LOW] `/Users/ahmedzaidan/Developer/SCMS-Simulator/crates/v2xw-world/src/serde_vwp.rs`:308

**Problem.** The f32 precision warning is raised only for lane centreline x and y (the only two `warn(...)` calls are at lines 409 and 414). Lane z, building rings, land-use rings, junction positions, signal-head positions, sites and crossings all narrow to f32 silently, so a world whose buildings or infrastructure lie beyond F32_MM_GRID_LIMIT_M loses millimetres with an empty `precision_warnings`. The same doc comment also states the limit as "about 4 km" (line 38 and line 130) where the constant is 16 384 m and quant.rs documents it as 16 km.

**Proposed fix.** Call `warn` from the shared put_ring closure and from each array-of-struct record writer, or scan World::bbox once against F32_MM_GRID_LIMIT_M and warn for the whole payload; and correct the two "4 km" comments to 16 km.

### R15 [LOW] `/Users/ahmedzaidan/Developer/SCMS-Simulator/crates/v2xw-world/src/serde_vwp.rs`:493

**Problem.** Two silent-truncation paths in the writer. (1) The junction `lane_count` column is written as `u16::try_from(...).unwrap_or(U16_NONE)`, so a junction touching 65 535 or more lanes writes 0xFFFF - which vwp-v1 §0 defines as the u16 "absent" sentinel, so an overflow is indistinguishable from "not stated" rather than from "very many". (2) `body_len`, every `off_*` and every count are written with `as u32` casts (line 303 onward), so a payload above 4 GiB wraps and produces a structurally corrupt file instead of an error. Neither is reachable on any realistic world (Manhattan's payload is 2 MB), but both fail silently rather than loudly.

**Proposed fix.** Saturate lane_count to 0xFFFE so the sentinel stays unambiguous, and return WorldError::Unrepresentable when body_len, an offset or a count exceeds u32::MAX.

## Visual and statistical review — 12 items

### V1 [HIGH]

**Defect.** 59.3 % of the world's "buildings" are OSM `building:part` fragments imported as independent buildings, double-counting the volume of their parent `building=*` outline. Building count inflated 2.44x (7390 against 3023 real structures), the obstacle R-tree carries 2.4x the entries it needs, the height distribution is skewed by parts that are taller than their parent's main mass, and any obstacle model that sums attenuation per intersected building will count the same wall two or three times.

**Evidence.** crates/v2xw-world/src/osm.rs:767-771 — `fn is_building` returns true when EITHER `building` OR `building:part` is set: `(building != "no" && !building.is_empty()) || (part != "no" && !part.is_empty())`. Counting the extract independently: of 7368 closed candidate building ways, 4370 (59.3 %) carry `building:part` and no `building` tag; only 2998 are real `building=*` ways, plus 25 building multipolygon relations. 7368 + 25 = 7393 against the 7390 the importer reports, the 3 difference being the 3 `no-outer-ring` anomalies — so this model of the importer's behaviour is exact. The tallest "building" in the world, 443.2 m, is OSM way 137425145, a `building:part=yes` with `min_height=330` and `roof:height=113.2`: the Empire State Building's spire imported as a building in its own right.

**Proposed fix.** In `is_building`, accept `building:part` only when the polygon is not contained by a `building=*` polygon (OSM Simple 3D Buildings says a part SUBDIVIDES its parent, it does not add to it). Better: give `Building` a `parts: Vec<BuildingPart>` field and attach parts to their parent, keeping the 3D detail without inflating the obstacle set. The one-line stopgap is to drop part-only ways and note the count as an anomaly.

### V2 [HIGH]

**Defect.** Underground subway-station polygons are imported as above-ground buildings with a defaulted 10 m height, placed directly on top of the roadway. Three phantom 10 m obstacles totalling 205,361 m2 (5.5 % of the requested bbox area) now sit on Times Square, Herald Square and Park Avenue at Grand Central — precisely the three intersections where a V2X study would want to model NLOS propagation, and where the model will now report blockage that does not exist.

**Evidence.** 1034 of 9711 drivable lane centreline vertices fall inside a building footprint; 878 of them (85 %) are inside just three polygons: building 7386 "Grand Central Terminal" (157,223 m2, height 10.0 m, HeightSource::Defaulted), 7385 "Times Square–42nd Street/Port Authority Bus Terminal" (31,825 m2, 10.0 m), 6826 "34th Street–Herald Square" (16,313 m2, 10.0 m). Their source tags are unambiguous — OSM way 812938420 carries `building=train_station`, `location=underground`, `layer=-1`, `underground=yes`, `railway=station`; relation 11171765 carries `building=train_station`, `location=underground`, `layer=-1`. `grep -n 'location\|underground\|layer' crates/v2xw-world/src/osm.rs` finds no handling of any of these keys. The extract contains 214 `location=underground` ways in all. The remaining overlaps ARE legitimate: the Helmsley Building (172 m, 50 vertices) genuinely has Park Avenue running through its arches, and building 3051 is the Grand Central head house the viaduct wraps.

**Proposed fix.** In the building classifier, skip a way or relation whose tags say it is below ground — `location=underground`, `underground=yes`, a negative `layer`, or a negative `building:min_level` with no positive `height`. If the geometry is wanted for other purposes, import it with `height_m = 0` and a flag that excludes it from the obstacle index rather than letting it default to 10 m.

### V3 [HIGH]

**Defect.** The world's origin and extent are determined by whichever kept way happens to overhang the requested box furthest, so every metre coordinate — and therefore the content hash — moves if the extract is re-fetched at a slightly different radius. For a project whose stated premise is byte-identical outputs across platforms and runs, the world frame itself is not reproducible from the scenario's own bbox.

**Evidence.** Origin is 40.7406720, -73.9955216: 370 m south and 467 m west of the requested SW corner (40.744, -73.990). The latitude comes from a single FDR Drive lane ending at 40.74072 and the longitude from the West 48th Street way starting at -73.9955216 — both incidental overhangs. World extent 3501 x 2659 m = 9.31 km2 against the requested 3.71 km2 (2.51x), because the Queens-Midtown Tunnel way runs 1.6 km east under the East River into Queens. `OsmOptions::bbox` cannot fix it: it is a whole-way KEEP FILTER, not a clip (crates/v2xw-world/src/osm.rs:4125-4131 via `way_in_bbox`, and the provenance records the rule verbatim as "a way is kept whole when any of its nodes is inside"). Since Overpass already returns exactly the ways intersecting the bbox, setting the option to the extract's own `<bounds>` produced a byte-identical world — both renders hashed 3c1c307d8cc6251d0aa01607e945b2dffbc4452fa113e7e8bade74f6410139cb, identical counts (13760 lanes, 7390 buildings), identical 3501 x 2659 m extent, identical 28 out-of-box lanes. So there is currently no way at all to bound a world's extent.

**Proposed fix.** Two independent changes, both small. (a) When `OsmOptions::bbox` is set, pin the origin to the REQUESTED box's south-west corner instead of deriving it from kept geometry — one line where `origin = GeoOrigin::new(floor_to_grid(min_lat, ...), ...)` is built around osm.rs:4197. That removes the origin instability on its own. (b) Add a real clip mode that truncates a polyline at the box boundary, interpolating the crossing point, so the extent is the request; keep the current keep-whole behaviour as the other arm of an explicit `ClipMode` enum so the choice is recorded in provenance rather than implied.

### V4 [HIGH]

**Defect.** Default speed limits are SUMO's free-flow DESIGN speeds, not urban legal limits, so 404 drivable lanes (16.7 %) carry a limit 2.5-3.5x the real one: 393 Midtown side-street lanes at 100 km/h and 11 at 142 km/h. A car-following model handed this world will drive 100 km/h down West 49th Street.

**Evidence.** The speed histogram shows 100 km/h on lanes named "West 49th Street", "West 45th Street", "East 46th Street", "West 50th Street", "East 45th Street", "West 53rd Street", and 142 km/h on 11 unnamed lanes. This cannot come from the data: the whole extract's maxspeed values top out at "60 mph" (456 ways are "25 mph", 322 are "10 mph"). The values are the class defaults in HIGHWAY_TABLE at crates/v2xw-world/src/osm.rs:1117-1131 — 27.78 m/s = 100.01 km/h for trunk/primary/secondary and 39.44 m/s = 141.98 km/h for motorway. mph parsing itself is correct: the 1480 tagged lanes come out at 40.23 km/h = 25 mph exactly, and the 33 FDR Drive / tunnel lanes at 64 km/h = 40 mph. The table is already marked UNVERIFIED in the model card with a `TODO: calibrate`, but it fails silently and produces a plainly wrong world.

**Proposed fix.** Insert a jurisdiction default layer ahead of the class default: read `maxspeed:type` / `source:maxspeed` (OSM writes `US-NY:city` here), and fall back to a per-country urban default (25 mph / 40 km/h for a US city) before falling back to the free-flow class speed. Record which of the three tiers supplied each lane's limit, the way `HeightSource` already does for buildings, so the calibration pass can target exactly the defaulted lanes.

### V5 [MEDIUM]

**Defect.** Ways that overhang the requested box carry BROKEN TOPOLOGY into the world: they are imported whole while their cross streets are not, producing long lanes that cross several avenues with no junction. A router or mobility model will drive 814 m through four intersections that do not exist.

**Evidence.** Lane 22 is an 814.1 m `residential` lane named "West 48th Street" running from 40.76461, -73.99550 (the world's own NW corner) to 40.76106, -73.98707, with zero predecessors and no intermediate junction. Verified in the source rather than assumed: OSM way 5671115's 13 nodes are shared by no other drivable way except its two ends and one interior node, because Ninth, Tenth, Eleventh and Twelfth Avenues are ABSENT from the extract — the drivable avenue set is 1st, 2nd, 3rd, 5th, 6th, 7th, 8th, Broadway, Lexington, Madison, Park, Park Avenue South, Vanderbilt and two tunnels, with no 9th-12th. So the importer behaved correctly; there was nothing to split at. 28 drivable lanes (2.37 km) lie wholly outside the requested bbox. 94.1 % of the extract's 58,434 nodes are inside the requested box, so 3440 are not.

**Proposed fix.** The same clip as defect 3. A geometric clip at the box boundary removes this geometry entirely rather than keeping a topologically wrong version of it. Until then, flag any non-motorway lane longer than ~250 m that has no intermediate junction as an anomaly, so it is visible in the import report instead of being discovered by rendering.

### V6 [MEDIUM]

**Defect.** Every drivable lane is exactly 3.50 m wide: the OSM `width` tag is never read, so lane offsets, junction polygon extents and any lateral-position model are uniform across the network. An 18 m avenue and a 6 m service alley get identical per-lane geometry.

**Evidence.** Drivable lane width over 2421 lanes: min = p10 = p50 = p90 = p99 = max = 3.50 m, exactly `OsmOptions::lane_width_m`'s default. Junction polygons in the 4 px/m render are correspondingly sized from lane count alone.

**Proposed fix.** Read the way's `width` (and `width:lanes` where present) and divide by the lane count, falling back to `lane_width_m` when absent; count the fallbacks as an anomaly so the share is visible in the report. This also feeds the junction-area polygon, so the fix improves both.

### V7 [MEDIUM]

**Defect.** Dual-carriageway and dog-leg junctions are not merged, so one real intersection is represented as several junctions with connectors running between them. This inflates the junction count, splits a single signal group across multiple plans, and gives a mobility model spurious short lanes between the halves of one crossing.

**Evidence.** The importer prints `not implemented   merge-dog-leg-junctions` in its own report. Visible in manhattan-junction.png: a second junction polygon and a second fan of connectors about 35 m east of the main junction at the same crossing. Quantitatively: 454 junctions with 3+ drivable arms inside the requested bbox = 122/km2, against ~69/km2 of real street intersections for this avenue (~185 m) and street (~80 m) spacing. The signalised count is the tighter bound — 284 against ~258 expected, so only about 10 % over — which says the effect is real but modest, not pervasive.

**Proposed fix.** Implement the documented dog-leg merge: join two junctions whose connectors are mutually adjacent, whose separation is under `junction_join_m`, and whose arms belong to the same pair of named ways. `ImportOptions::junction_join_m` (default 10 m) already exists as the knob; the 35 m case needs a name-aware rule rather than a larger radius, or genuine four-way crossings of nearby parallel streets would be merged too.

### V8 [LOW]

**Defect.** 263 driving lanes (10.9 %) are shorter than 5 m and the shortest is 0.99 m — junction-trimming slivers left on real arterial approaches, not just driveway stubs. A car-following or lane-change model handed a 1 m lane has no room to act, and a vehicle can traverse one within a single time step.

**Evidence.** Drivable lane length p10 = 4.54 m, min 0.99 m. By road class: service 128, secondary 44, primary 43, residential 26, link 12, living 4, motorway 4, tertiary 2 — so 89 of them are on primary/secondary arterials, where a 1-5 m lane is clearly an artefact of junction trimming rather than a real geometry. The importer separately counts 49 `segment-too-short-to-trim` and 2 `degenerate-lane` anomalies, so the condition is partly detected but not resolved.

**Proposed fix.** After junction trimming, merge any non-internal lane shorter than a threshold (5 m is a reasonable start) into its unique predecessor or successor when it has exactly one of each, and count the merges. Where it cannot be merged, raise it as an anomaly rather than emitting it.

### V9 [LOW]

**Defect.** 3421 junctions exist but 2654 (77.6 %) have no drivable arm at all: they are footway intersections in the sidewalk mesh, carried in the same collection as road junctions. Every consumer that iterates junctions pays 4.5x, and the headline "303 of 3421 signalised (8.9 %)" badly understates signal coverage, which is really 61 % of the 466 junctions that have 3+ drivable arms.

**Evidence.** Junction degree histogram over drivable edge arms: degree 0 = 2654 (77.6 %). Visible in manhattan-broadway.png as small grey square markers at every footway intersection along the green sidewalk mesh.

**Proposed fix.** Either keep pedestrian-only junctions in a separate collection, or add a cheap predicate/flag on `Junction` (e.g. a cached count of drivable arms) so callers can filter without walking the edge list. The import report should quote signalisation against junctions with 3+ drivable arms, not against all junctions.

### V10 [LOW]

**Defect.** Building `height` is taken to the tip, including antenna masts and spires, and the resulting thin prism is indistinguishable from solid building mass in the obstacle model. A ray traced across Midtown will be blocked by an antenna.

**Evidence.** The world's tallest building is 443.2 m — OSM way 137425145, `building:part=yes`, `height=443.2`, `min_height=330`, `roof:height=113.2`: the Empire State Building's spire, whose roof is at 381 m. Under Simple 3D Buildings this is geometrically correct, and `Building::min_height_m` does carry the 330 m, so the data is not lost; the risk is in the consumer, not the importer.

**Proposed fix.** Nothing to change in the importer beyond defect 1. When the obstacle model is written, document that a building whose `min_height_m` is a large fraction of its `height_m` is a spire or a mast, and either exclude it or attenuate by its actual cross-section rather than treating it as a full-height prism.

### V11 [INFO]

**Defect.** NOT A DEFECT, but the task's premise is wrong and worth recording: Central Park is not in this extract, so its absence from the render is correct. The requested bbox tops out at latitude 40.762 and Central Park South is at 40.7644 — about 265 m further north.

**Evidence.** Searching the extract for "Central Park" across all ways and relations returns exactly three matches, none of them the park: OSM way 265943081 named "Central Parking", and two bus-route relations (3006209, 12765646) naming "Central Park South" as a route destination. The world's max latitude of 40.764618 is set by the West 48th Street overhang of defect 5, not by park geometry. The extract is also 1858 x 1999 m, not the 2.4 x 2.0 km the task states.

**Proposed fix.** None needed here. If Central Park's southern edge is wanted as a landmark for future renders, re-fetch with maxlat 40.768 or higher.

### V12 [INFO]

**Defect.** NOT A DEFECT, but a trap I fell into and corrected, worth recording so nobody else reads the number the wrong way: the lane-level strongly-connected-component figure of 52.8 % looks alarming and means almost nothing.

**Evidence.** A lane change is not a `Connection`, so a lane restricted to a single movement is its own strong component; the lane graph therefore has 2422 strong components and the largest holds only 1278 of 2421 driving lanes. Restricting to street classes moves it to 51.9 %, which disproves the obvious explanation (service spurs). At the EDGE level, which is what reachability means once lateral lane changes exist, 1007 of 1216 drivable edges (82.8 %) are reachable both ways from the busiest edge, carrying 123.45 of 163.57 lane km — plausible for a 1.9 x 2.0 km cut out of a one-way grid. Separately, my first run reported 2459 weak components and 2420 lanes with no successor: that was a bug in my own traversal, not the crate's. A movement is recorded twice — `approach -> departure` carrying its connector in `via`, and `connector -> departure` with `via` empty — so the geometric hops are `from_lane -> via` and then `via -> to_lane`. Taking only the `via`-empty records leaves every approach lane with no successor.

**Proposed fix.** Report edge-level reachability, not lane-level SCC, whenever the question is "can a vehicle get there". If a lane-level figure is wanted, model lane changes as explicit lateral adjacency first. Worth a doc note on `Connection` spelling out which of the two records a traversal should follow, since the current wording is correct but easy to misread.

## Verdicts

**Code reviewer.** The crate is in much better shape than an adversarial pass usually finds, and its two headline claims survive hard testing: the double import of the real 30 MB Manhattan extract is byte-identical across all four artefacts, and an independently written TypeScript encoder reproduces the 2 MB vwp-world/1 payload byte for byte. Determinism discipline is real, not asserted - no HashMap or HashSet anywhere, no std transcendental, f32 only on the wire, ids sorted at the parser, spatial-index answers exactly equal to brute force. Robustness is real too: 16 malformed inputs, no panic, errors where errors belong and counted anomalies elsewhere. I did not refute the crate's correctness as a whole, but I found 15 defects, one of them serious. The one that matters is geometric, not procedural: `offset_polyline`'s mitre cap inverts a lane at a bend tighter than its own offset, and 56 lanes in the Phase 1 world - 8 of them driving lanes - have centrelines that cross themselves. Arc length stops being injective in space, so one point on lane l7790 has two arc lengths 9.52 m apart and `project_point` returns only one of them; every gap, leader and lateral offset a mobility model computes from (lane, s) on those lanes is wrong, and nothing in Lane::new, World::validate or the 30-category anomaly list notices. Fix that before the mobility tier is built on top of it. Below it sit four medium semantic gaps worth fixing next - a vehicle `maxspeed` becoming a footway's walking speed (7 real Manhatt

**Visual reviewer.** The imported Manhattan world is GEOMETRICALLY CORRECT in every respect I could test. The projection, the frame orientation, the grid rotation, lane offsetting and handedness, junction placement and building placement are all right, several of them confirmed to within 1-2 %. The defects I found are attribute and scope defects, not geometry defects — but four of them are serious enough to fix before this world is used for anything quantitative.

WHAT IS PROVEN RIGHT. The projection agrees with a Vincenty geodesic to 0.110 m worst case over baselines up to 856 m (+0.012 % uniform scale bias, the documented shear of an equirectangular tangent plane, not noise). The grid's modal drivable heading is 60-61 deg mod 90 with 90.5 % of lane length within +/-4 deg, i.e. the Commissioners' Plan 28.9 deg rotation to within a degree; Broadway measures 81 deg mod 90, a genuine 20 deg diagonal, and appears in the render exactly where and at what angle it should, broken only where it is legally pedestrianised. All three landmark crosshairs land on the correct blocks, the Grand Central one squarely on the Park Avenue Viaduct loop. North-up and east-right were re-derived independently. Right-hand traffic is correct on 610 of 610 multi-lane edges and 178 of 178 two-way street pairs, zero wrong. Lanes are parallel and evenly offset with no crossing or tangling anywhere in either close-up. Junction polygons sit on the intersections, correctly sized and aligned. Buildings sit inside blocks. Roadway 
