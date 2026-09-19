# World importer defects found by visual and statistical verification

Found 2026-09-18 by reviewing `worlds/cache/render-report.txt` and the Midtown
Manhattan renders against the real city. The geometry and projection are
**verified correct**; these are semantic-attribution defects.

## What verification proved correct

- **Projection.** Worst pairwise geodesic-vs-projected error is 0.110 m over an
  856 m baseline (0.013 %), checked against three surveyed landmarks. Raster is
  north-up and east-right.
- **Grid orientation.** Modal drivable heading is 60–61° (mod 90) with 90.5 % of
  lane length within ±4° of it. Manhattan's grid is rotated ~29° from true
  north, so 60° from east is exactly right. Broadway is correctly the outlier at
  81°, the famous diagonal.
- **Lane-level structure.** The close-up render shows per-lane centrelines,
  crossings with stop lines, flanking sidewalks, turn connections curving
  through junctions and consistent one-way chevrons.
- **Building heights.** Max 443.2 m, p50 45 m — consistent with Midtown.

## W1 (major) — speed-limit class defaults are German rural values

`HIGHWAY_TABLE` in `crates/v2xw-world/src/osm.rs:1117` uses SUMO's netconvert
German defaults: motorway 39.44 m/s (142 km/h), trunk/primary/secondary
27.78 m/s (100 km/h), tertiary/residential 13.89 m/s (50 km/h).

Applied unmodified to Manhattan this gives **393 drivable lanes (16 %) a limit of
100 km/h**, including named side streets (West 49th, East 46th, West 50th), and
11 lanes at 142 km/h. Tag parsing is correct (25 mph -> 40.23 km/h, 40 mph ->
64 km/h), so only the *fallback* is wrong.

Impact: every mobility, safety-envelope and Doppler result on 16 % of the network
is derived from a limit 2.5x too high.

Fix: make the table a **named, swappable preset** with a cited source per row,
not a hard-coded constant. Ship at least `sumo-german` (today's values, for
reproducing SUMO) and `urban-us` (NYC citywide default 25 mph = 40.2 km/h), and
select it from the scenario. This is required by the no-black-box rule: the
current numbers are silently jurisdiction-specific with no source cited.

## W2 (major) — lane width is a single global constant

`osm.rs:1665` sets `lane_width_m` from `options.lane_width_m` for every motor
way, so the statistics show min = p10 = p50 = p90 = p99 = max = 3.50 m. Per-way
`width` and `lanes:width` tags are never consulted for motor lanes (the `width`
parse at `osm.rs:3548` serves another path). Manhattan avenue lanes are ~3.0–3.35 m.

Impact: corridor half-widths, lane offsets and therefore every lateral position
and sidewalk placement carry a systematic error.

## W3 — RETRACTED. The strong-connectivity figure is close to meaningless

**I got this wrong.** I originally recorded "only 52.8 % of driving lanes are
strongly connected, across 2,422 strong components" as a major routing defect.
An independent reviewer refuted it, and the refutation is correct.

A lane change is not a `Connection`. The lane graph's edges are turn movements
only, so a lane whose sole permitted movement leads onward is its own strongly
connected component by construction. In a largely one-way grid clipped to a
small bounding box, the turn graph is close to acyclic at lane granularity, so
thousands of singleton components are the expected result, not a symptom.

The meaningful connectivity measure is the weak one, and it is healthy: 56
components with the largest holding 95.6 % of lanes. What survives from the
original observation is much narrower: 70 driving lanes have no successor and
251 no predecessor, most of which is bounding-box clipping (see W5) rather than
a topology defect.

Recorded rather than deleted, because the reasoning error is worth keeping: a
graph metric was quoted without checking what the graph's edges actually mean.

## W4 (medium) — subway-station polygons are treated as buildings

10.648 % of lane vertices (1034 of 9711) fall inside a building footprint. The
top offenders are transit polygons at their default 10.0 m height: Grand Central
Terminal (438 vertices), Times Square–42nd/Port Authority (226), 34th
Street–Herald Square (214).

Impact: roads legitimately pass over these. Treating them as 10 m buildings will
inject spurious wall attenuation into the Sommer obstacle model for every link
crossing them, biasing packet delivery in the busiest part of the map.

Fix: exclude `railway=station`/`public_transport` polygons without a real
building tag from the obstacle set, or give them a height of 0 and record them as
land use.

## W5 (medium) — whole ways are kept if any node is in the bbox

The world spans 3501 x 2659 m (9.31 km2) against a requested 1858 x 1999 m
(3.71 km2), i.e. **2.51x the requested area**, with stray lanes trailing far
off-map (visible as long thin lines to the map corners). 28 drivable lanes lie
wholly outside the requested bbox.

Impact: inflates the ENU extent, the uniform spatial grid and the index cell
count for no modelling benefit.

Fix: offer bbox clipping of way geometry as an option, defaulting to clip with a
small margin, and record the choice in `WorldProvenance`.

## W6 (minor) — `merge-dog-leg-junctions` is declared but not implemented

The importer reports this simplification as not implemented. Left as a known gap.
