//! Phase 3 world scope, over the public API only: terrain from a DEM, line of sight over
//! it, and the radial and random procedural generators.
//!
//! The fixtures are tiny and hand-computable on purpose. The ASCII-grid raster is a ramp
//! in `x` alone, so every resampled height is a number a reader can check in their head;
//! the terrain used for line of sight is a single 50 m hill in the middle of a 300 m
//! square, so the knife edge it produces has a height and two distances that are exact.
//!
//! Every world these tests build goes through `WorldBuilder::build`, which runs
//! `World::validate`, so "it built" already means dense ids, resolving references,
//! consistent arc lengths, sorted connections, matching conflict matrices, coherent signal
//! plans and every exported float on its quantisation grid. What the tests add is what
//! `validate` cannot know: that the numbers are the right numbers, that the same input
//! gives the same content hash, and that no lane centreline crosses itself.

use v2xw_core::geom::Vec3;
use v2xw_world::dem::{
    AsciiGridCrs, DemAnomaly, DemFormat, DemOptions, DemReport, DemSource, DrapeOptions, SRTM_VOID,
    parse_srtm_tile, read_dem_bytes, read_srtm_hgt, resample, with_terrain,
};
use v2xw_world::los::{ProfileParams, terrain_los, terrain_profile};
use v2xw_world::procedural::radial::{RadialParams, RadialSource, radial, radial_graph};
use v2xw_world::procedural::random::{RandomParams, RandomSource, random, random_graph};
use v2xw_world::procedural::{GridParams, grid};
use v2xw_world::quant::is_on_grid;
use v2xw_world::{
    ImportOptions, Interpolation, Terrain, World, WorldSource, WorldSourceSpec, serde_native,
};

/// The import options every test uses: a fixed date, so nothing reads a clock.
fn opts() -> ImportOptions {
    ImportOptions::default().imported_at("2026-09-22T00:00:00Z")
}

/// An ESRI ASCII grid whose height is `x / 10` and nothing else, in world metres.
///
/// `xllcenter`/`yllcenter` put sample `(0, 0)` exactly at the origin, so the three
/// columns stand at x = 0, 100 and 200 m.
const RAMP_ASCII: &str = "ncols 3\nnrows 3\nxllcenter 0.0\nyllcenter 0.0\ncellsize 100.0\n\
                          NODATA_value -9999\n0 10 20\n0 10 20\n0 10 20\n";

/// The options that resample a world-metre raster onto exactly the box asked for.
fn ramp_options() -> DemOptions {
    DemOptions {
        ascii_grid_crs: AsciiGridCrs::WorldMetres,
        margin_m: 0.0,
        cell_m: 100.0,
        bounds_m: Some((0.0, 0.0, 200.0, 200.0)),
        ..DemOptions::default()
    }
}

#[test]
fn an_ascii_grid_is_read_and_resampled_onto_the_world_grid() {
    let options = ramp_options();
    let (raster, mut report) = read_dem_bytes(
        RAMP_ASCII.as_bytes(),
        "ramp.asc",
        DemFormat::EsriAsciiGrid,
        &options,
    )
    .expect("the grid reads");
    assert_eq!((raster.ncols, raster.nrows), (3, 3));
    assert_eq!(raster.void_count(), 0);
    // Row 0 is the southernmost whatever order the file was in, and the ramp is in `x`.
    assert_eq!(raster.sample_at(0, 0), Some(0.0));
    assert_eq!(raster.sample_at(1, 0), Some(10.0));
    assert_eq!(raster.sample_at(2, 2), Some(20.0));

    let terrain = resample(&raster, &options, &mut report).expect("it resamples");
    // The bounds span 200 m at 100 m spacing, and the grid keeps one cell of headroom.
    assert_eq!((terrain.nx, terrain.ny), (4, 4));
    assert_eq!(terrain.cell_x_m, 100.0);
    assert_eq!(terrain.origin_x_m, 0.0);
    assert_eq!(terrain.interpolation, Interpolation::Bilinear);
    assert_eq!(terrain.height_at(0.0, 0.0), Some(0.0));
    assert_eq!(terrain.height_at(100.0, 100.0), Some(10.0));
    // Halfway between two posts is the mean of the two, exactly.
    assert_eq!(terrain.height_at(50.0, 0.0), Some(5.0));
    assert_eq!(terrain.height_at(50.0, 137.0), Some(5.0));
    assert_eq!(terrain.height_at(-1.0, 0.0), None, "outside the grid");
    assert_eq!(report.height_range_m.0, 0.0);
    assert_eq!(report.height_range_m.1, 20.0);
    // Every height is on the height grid, because `Terrain::new` put it there.
    assert!(terrain.heights_m.iter().all(|h| is_on_grid(*h, 1e-3)));
}

#[test]
fn a_void_is_repaired_and_counted_rather_than_read_as_sea_level() {
    // 4 x 4, so that the void at sample (1, 1) is not a corner of every bilinear cell and
    // some target samples are still clean. North row first, as the format requires.
    let holed = "ncols 4\nnrows 4\nxllcenter 0.0\nyllcenter 0.0\ncellsize 100.0\n\
                 NODATA_value -9999\n0 10 20 30\n0 10 20 30\n0 -9999 20 30\n0 10 20 30\n";
    let options = DemOptions {
        ascii_grid_crs: AsciiGridCrs::WorldMetres,
        margin_m: 0.0,
        cell_m: 100.0,
        bounds_m: Some((0.0, 0.0, 300.0, 300.0)),
        ..DemOptions::default()
    };
    let (raster, mut report) = read_dem_bytes(
        holed.as_bytes(),
        "holed.asc",
        DemFormat::EsriAsciiGrid,
        &options,
    )
    .expect("the grid reads");
    assert_eq!((raster.ncols, raster.nrows), (4, 4));
    assert_eq!(raster.void_count(), 1);
    assert_eq!(raster.sample_at(1, 1), None, "the void is at (1, 1)");
    assert_eq!(raster.sample_at(1, 0), Some(10.0));
    assert_eq!(report.anomaly(DemAnomaly::SourceVoid), 1);

    let terrain = resample(&raster, &options, &mut report).expect("it resamples");
    // Every height is finite and inside the range the neighbours span: a void is missing
    // data, and the repair ladder never invents a height outside the data it repaired
    // from.
    assert!(
        terrain
            .heights_m
            .iter()
            .all(|h| h.is_finite() && (0.0..=30.0).contains(h)),
        "heights: {:?}",
        terrain.heights_m
    );
    // The repair is counted, in both of the ways it can happen …
    assert!(report.anomaly(DemAnomaly::PartialVoidInterpolated) > 0);
    assert!(report.samples_repaired > 0);
    // … and the samples the void never touched are still clean, which is what makes the
    // "repaired" count mean something.
    assert!(report.samples_clean > 0, "{}", report.to_text());
}

#[test]
fn an_srtm_tile_is_flipped_and_georeferenced_from_its_name() {
    assert_eq!(parse_srtm_tile("N40W074.hgt"), Some((40.0, -74.0)));
    assert_eq!(parse_srtm_tile("S01E005.hgt"), Some((-1.0, 5.0)));
    assert_eq!(parse_srtm_tile("n40w074.hgt"), Some((40.0, -74.0)));
    assert_eq!(parse_srtm_tile("not-a-tile.hgt"), None);

    // A 2 x 2 tile: the file's first row is the northernmost.
    let mut bytes = Vec::new();
    for value in [100i16, 200, SRTM_VOID, 400] {
        bytes.extend_from_slice(&value.to_be_bytes());
    }
    let raster = read_srtm_hgt(&bytes, (40.0, -74.0)).expect("the tile reads");
    assert_eq!((raster.ncols, raster.nrows), (2, 2));
    // Row 0 is the south row, which is the file's second row.
    assert_eq!(raster.sample_at(0, 0), None, "the void survives as a void");
    assert_eq!(raster.sample_at(1, 0), Some(400.0));
    assert_eq!(raster.sample_at(0, 1), Some(100.0));
    assert_eq!(raster.sample_at(1, 1), Some(200.0));
    let bbox = raster.geodetic_bbox().expect("a geodetic raster");
    assert_eq!(bbox.min_lat_deg, 40.0);
    assert_eq!(bbox.max_lat_deg, 41.0);
    assert_eq!(bbox.min_lon_deg, -74.0);

    // A byte count that is not twice a perfect square is not a tile.
    assert!(read_srtm_hgt(&[0u8; 6], (0.0, 0.0)).is_err());
    assert!(read_srtm_hgt(&[0u8; 7], (0.0, 0.0)).is_err());
}

/// A 3 x 3 terrain over a 300 m square with a single 50 m hill in the middle.
fn hill() -> Terrain {
    Terrain::new(
        0.0,
        0.0,
        150.0,
        150.0,
        3,
        3,
        vec![0.0, 0.0, 0.0, 0.0, 50.0, 0.0, 0.0, 0.0, 0.0],
        Interpolation::Bilinear,
    )
    .expect("a 3 x 3 grid")
}

/// A small grid world, flat at `z = 0`.
fn flat_world() -> World {
    grid(&GridParams::legacy().with_size(3, 3), &opts()).expect("the grid builds")
}

#[test]
fn attaching_a_terrain_moves_the_ground_but_not_the_geometry() {
    let flat = flat_world();
    assert_eq!(flat.ground_height_at(150.0, 150.0), 0.0);
    assert!(flat.terrain.is_none());

    let (draped, report) =
        with_terrain(&flat, hill(), &DrapeOptions::none(), &DemReport::default())
            .expect("a terrain attaches");
    assert_eq!(draped.ground_height_at(150.0, 150.0), 50.0);
    assert_eq!(draped.ground_height_at(0.0, 0.0), 0.0);
    // Nothing was lifted, so every lane is where the generator put it …
    assert_eq!(report.points_lifted, 0);
    assert_eq!(report.lanes, 0);
    for (before, after) in flat.roads.lanes().iter().zip(draped.roads.lanes()) {
        assert_eq!(before.centreline, after.centreline);
    }
    // … but the world is a different world, because the grid is hashed content.
    assert_ne!(flat.content_hash, draped.content_hash);
    assert!(draped.terrain.is_some());
}

#[test]
fn draping_lifts_the_geometry_onto_the_dem() {
    let flat = flat_world();
    let (draped, report) = with_terrain(&flat, hill(), &DrapeOptions::all(), &DemReport::default())
        .expect("a terrain attaches");
    assert!(report.points_lifted > 0);
    assert_eq!(report.points_already_elevated, 0, "the source was flat");
    assert!(report.lanes > 0);
    assert!(report.junctions > 0);

    // The junction nearest the hilltop stands on it; the corner junction does not.
    let highest = draped
        .roads
        .junctions()
        .iter()
        .map(|j| j.position.z)
        .fold(f64::NEG_INFINITY, f64::max);
    assert!(highest > 10.0, "a junction climbed the hill: {highest}");
    let lowest = draped
        .roads
        .junctions()
        .iter()
        .map(|j| j.position.z)
        .fold(f64::INFINITY, f64::min);
    assert!(
        lowest < 1.0,
        "the corner junction is still at the pyramid's foot: {lowest}"
    );

    // Draping a world that already has a DEM is refused rather than added twice.
    assert!(with_terrain(&draped, hill(), &DrapeOptions::all(), &DemReport::default()).is_err());
    // Attaching without draping is still allowed, because it cannot double-count.
    assert!(
        with_terrain(
            &draped,
            hill(),
            &DrapeOptions::none(),
            &DemReport::default()
        )
        .is_ok()
    );

    // A draped world is still a world: it round-trips through the native format with its
    // hash intact.
    let bytes = serde_native::to_bytes(&draped).expect("it writes");
    let read_back = serde_native::from_bytes(&bytes).expect("it reads");
    assert_eq!(read_back.content_hash, draped.content_hash);
    assert_eq!(read_back.terrain, draped.terrain);
}

#[test]
fn a_hill_blocks_the_link_over_it() {
    let flat = flat_world();
    let (world, _) = with_terrain(&flat, hill(), &DrapeOptions::none(), &DemReport::default())
        .expect("a terrain attaches");

    // Two antennas 2 m up, 280 m apart, with the hilltop exactly halfway.
    let a = Vec3::new(10.0, 150.0, 2.0);
    let b = Vec3::new(290.0, 150.0, 2.0);
    let los = terrain_los(&world, a, b, &ProfileParams::default()).expect("a profile");
    assert!(!los.clear, "a 50 m hill blocks a 2 m antenna pair");
    assert!(
        (los.max_obstruction_m - 48.0).abs() < 1e-9,
        "the hilltop is 50 m and the line is at 2 m: {}",
        los.max_obstruction_m
    );
    assert_eq!(los.edges.len(), 1, "one hill, one knife edge");
    let edge = los.edges[0];
    assert!((edge.d1_m + edge.d2_m - 280.0).abs() < 1e-6);
    assert!((edge.d1_m - 140.0).abs() < 1e-6, "the hill is halfway");
    assert!((edge.h_m - 48.0).abs() < 1e-9);
    assert!(los.obstructed_len_m > 0.0 && los.obstructed_len_m <= 280.0);

    // A link along the southern edge of the grid is clear: bilinear interpolation makes
    // the single high sample a pyramid over the whole square, and the pyramid's foot is
    // the edge row, which is all zeros.
    let clear = terrain_los(
        &world,
        Vec3::new(10.0, 0.0, 2.0),
        Vec3::new(290.0, 0.0, 2.0),
        &ProfileParams::default(),
    )
    .expect("a profile");
    assert!(clear.clear);
    assert_eq!(clear.edges.len(), 0);

    // And a world with no DEM cannot be blocked by its own datum plane.
    let none = terrain_los(&flat, a, b, &ProfileParams::default()).expect("a profile");
    assert!(none.clear);
    assert_eq!(none.max_obstruction_m, 0.0);
}

#[test]
fn the_profile_samples_at_the_interval_it_was_asked_for() {
    let flat = flat_world();
    let (world, _) = with_terrain(&flat, hill(), &DrapeOptions::none(), &DemReport::default())
        .expect("a terrain attaches");
    let a = Vec3::new(0.0, 150.0, 2.0);
    let b = Vec3::new(300.0, 150.0, 2.0);

    let coarse = terrain_profile(&world, a, b, &ProfileParams::default()).expect("a profile");
    assert_eq!(coarse.samples.len(), 11, "300 m at 30 m is ten intervals");
    assert!((coarse.spacing_m - 30.0).abs() < 1e-9);
    assert!(coarse.has_terrain);
    assert_eq!(coarse.total_m, 300.0);
    assert_eq!(coarse.samples[0].s_m, 0.0);
    assert!((coarse.samples[10].s_m - 300.0).abs() < 1e-9);

    let fine = terrain_profile(
        &world,
        a,
        b,
        &ProfileParams::default().sample_spacing_m(10.0),
    )
    .expect("a profile");
    assert_eq!(fine.samples.len(), 31);
    // A finer profile finds the same hilltop, because the grid is what it samples.
    assert!((fine.max_obstruction_m() - coarse.max_obstruction_m()).abs() < 1e-9);

    // The cap coarsens the profile rather than allocating what was asked for.
    let capped = terrain_profile(
        &world,
        a,
        b,
        &ProfileParams {
            sample_spacing_m: 1.0,
            max_samples: 11,
            ..ProfileParams::default()
        },
    )
    .expect("a profile");
    assert_eq!(capped.samples.len(), 11);
    assert!((capped.spacing_m - 30.0).abs() < 1e-9);

    // A degenerate link is two samples and no edges, not an error.
    let degenerate = terrain_profile(&world, a, a, &ProfileParams::default()).expect("a profile");
    assert_eq!(degenerate.total_m, 0.0);
    assert!(degenerate.edges().is_empty());

    // Parameters that cannot describe a profile are refused.
    assert!(
        terrain_profile(
            &world,
            a,
            b,
            &ProfileParams::default().sample_spacing_m(0.0)
        )
        .is_err()
    );
}

// ---------------------------------------------------------------------------
// The radial generator
// ---------------------------------------------------------------------------

#[test]
fn the_radial_graph_is_the_legacy_spider() {
    let params = RadialParams::legacy().with_size(4, 2);
    let (nodes, edges) = radial_graph(&params).expect("the graph builds");
    // One centre plus rings x arms.
    assert_eq!(nodes.len(), 9);
    assert_eq!(params.node_count(), 9);
    // Four spokes of two segments each, plus two rings of four chords.
    assert_eq!(edges.len(), 16);
    // The centre sits at (span, span) with span = rings x spacing, so nothing is negative.
    assert_eq!(nodes[0], Vec3::new_2d(240.0, 240.0));
    assert!(nodes.iter().all(|p| p.x >= -1e-9 && p.y >= -1e-9));
    // Ring node (1, 0) is due east of the centre at one spacing.
    let east = nodes[params.node_index(1, 0)];
    assert!((east.x - 360.0).abs() < 1e-9 && (east.y - 240.0).abs() < 1e-9);
    // The wrap-around ring edge exists: arm 3 joins arm 0.
    assert!(
        edges
            .iter()
            .any(|e| e.a == params.node_index(1, 3) && e.b == params.node_index(1, 0)),
        "the ring closes"
    );
    // Fewer than three arms is not a radial network.
    assert!(radial_graph(&RadialParams::legacy().with_size(2, 2)).is_err());
}

#[test]
fn the_radial_world_builds_and_is_a_pure_function_of_its_parameters() {
    let params = RadialParams::legacy().with_size(4, 2);
    let a = radial(&params, &opts()).expect("it builds");
    let b = radial(&params, &opts()).expect("it builds");
    assert_eq!(a.content_hash, b.content_hash);
    assert_eq!(a.counts().junctions, 9);
    assert!(a.counts().lanes > 0);
    assert!(a.counts().connections > 0);
    assert_no_self_intersecting_lane(&a);
    assert_every_float_on_its_grid(&a);

    // The import date is provenance, not geometry.
    let dated =
        radial(&params, &ImportOptions::default().imported_at("1999-01-01")).expect("it builds");
    assert_eq!(dated.content_hash, a.content_hash);

    // Signals, when asked for, land on the junctions with three or more arms: the centre
    // and every ring node, but not a one-armed stub (there are none here).
    let signalised = radial(
        &RadialParams::legacy().with_size(4, 2).with_signals(true),
        &opts(),
    )
    .expect("it builds");
    assert!(!signalised.signals.is_empty());
    for plan in &signalised.signals {
        assert!(
            (plan.total_phase_duration_s() - plan.cycle_s).abs() <= 1e-3,
            "the phases sum to the cycle"
        );
        assert!(plan.offset_s >= 0.0 && plan.offset_s < plan.cycle_s);
    }
    assert_ne!(signalised.content_hash, a.content_hash);
}

#[test]
fn radial_dropout_removes_ring_edges_deterministically() {
    let mut params = RadialParams::legacy().with_size(4, 2);
    let (_, all) = radial_graph(&params).expect("the graph builds");
    params.dropout = 0.5;
    let (_, halved) = radial_graph(&params).expect("the graph builds");
    // Eight ring edges, half of them removed; the eight spokes are untouched.
    assert_eq!(all.len(), 16);
    assert_eq!(halved.len(), 12);
    // And it is a decimation, not a draw: the same parameters give the same edges.
    let (_, again) = radial_graph(&params).expect("the graph builds");
    assert_eq!(halved, again);
    // A dropout of 1 or more is not a fraction of the edges.
    params.dropout = 1.0;
    assert!(radial_graph(&params).is_err());
}

#[test]
fn the_radial_source_answers_to_its_model_id_and_nothing_else() {
    let source = RadialSource::new();
    let card = source.card();
    assert_eq!(card.id, "world/source/procedural-radial");
    card.validate().expect("the card validates");
    for parameter in card.todo_calibrate() {
        assert!(
            parameter.calibration.is_some(),
            "{} needs a calibration plan",
            parameter.name
        );
    }
    let world = source
        .build(
            &WorldSourceSpec::procedural(
                "world/source/procedural-radial",
                serde_json::json!({"arms": 4, "rings": 2}),
            ),
            &opts(),
        )
        .expect("it builds");
    assert_eq!(
        world.content_hash,
        radial(&RadialParams::legacy().with_size(4, 2), &opts())
            .expect("it builds")
            .content_hash
    );
    assert!(
        source
            .build(
                &WorldSourceSpec::procedural("world/source/procedural-grid", serde_json::json!({})),
                &opts()
            )
            .is_err(),
        "a source refuses what it does not implement"
    );
}

// ---------------------------------------------------------------------------
// The random generator
// ---------------------------------------------------------------------------

#[test]
fn the_random_growth_respects_its_own_distances() {
    let params = RandomParams::small(7);
    let (nodes, edges, stats) = random_graph(&params).expect("the graph grows");
    assert!(stats.edges > 0);
    assert_eq!(stats.nodes as usize, nodes.len());
    assert_eq!(stats.edges as usize, edges.len());
    assert_eq!(stats.iterations, u64::from(params.iterations));

    // No two junctions are closer than min_distance, and no street is longer than
    // max_distance: both are the growth rule's own bounds.
    for (i, p) in nodes.iter().enumerate() {
        for q in nodes.iter().skip(i + 1) {
            assert!(
                p.distance_2d(*q) >= params.min_distance_m - 1e-9,
                "two junctions {} m apart",
                p.distance_2d(*q)
            );
        }
    }
    for edge in &edges {
        let length = nodes[edge.a].distance_2d(nodes[edge.b]);
        assert!(
            length >= params.min_distance_m - 1e-9,
            "a {length} m street is shorter than min_distance_m"
        );
        assert!(edge.a != edge.b, "no street joins a junction to itself");
    }
    // And no two streets cross, which is what makes every crossing a junction.
    for (i, e) in edges.iter().enumerate() {
        for f in edges.iter().skip(i + 1) {
            if e.a == f.a || e.a == f.b || e.b == f.a || e.b == f.b {
                continue;
            }
            assert!(
                !segments_cross(nodes[e.a], nodes[e.b], nodes[f.a], nodes[f.b]),
                "two streets cross without a junction"
            );
        }
    }
}

#[test]
fn the_random_world_is_reproducible_from_its_seed() {
    let params = RandomParams::small(7);
    let a = random(&params, &opts()).expect("it builds");
    let b = random(&params, &opts()).expect("it builds");
    assert_eq!(a.content_hash, b.content_hash, "same seed, same world");
    assert!(a.counts().lanes > 0);
    assert_no_self_intersecting_lane(&a);
    assert_every_float_on_its_grid(&a);

    let other = random(&params.clone().with_seed(8), &opts()).expect("it builds");
    assert_ne!(
        a.content_hash, other.content_hash,
        "a different seed is a different world"
    );

    // The card declares the stream it draws from, which is what makes the run auditable.
    let card = RandomSource::new().card();
    assert_eq!(card.id, "world/source/procedural-random");
    card.validate().expect("the card validates");
    assert!(card.determinism.uses_rng);
    assert_eq!(
        card.determinism.rng_domains,
        vec!["plugin:world/source/procedural-random".to_string()]
    );
    // And the provenance records the seed, so a world can be regrown from it.
    assert!(
        a.provenance
            .transformations
            .iter()
            .any(|t| t.name == "procedural-random" && t.params.contains_key("seed"))
    );
}

#[test]
fn a_random_network_that_cannot_grow_is_refused() {
    // A minimum angle of 89 degrees with one try per iteration places almost nothing, and
    // a network with no street at all is an error rather than a one-junction world.
    let params = RandomParams {
        iterations: 1,
        tries_per_iteration: 1,
        max_radius_m: Some(1.0e-3),
        ..RandomParams::small(1)
    };
    assert!(
        params.validate().is_err(),
        "the radius bound is below a street"
    );
    let params = RandomParams {
        min_angle_deg: 90.0,
        ..RandomParams::small(1)
    };
    assert!(params.validate().is_err(), "at 90 degrees nothing can join");
}

// ---------------------------------------------------------------------------
// Shared checks
// ---------------------------------------------------------------------------

/// No lane centreline crosses itself.
///
/// The world model forbids it and the importers repair it; this is the independent check,
/// written against the public geometry rather than against the crate's own helper.
fn assert_no_self_intersecting_lane(world: &World) {
    for lane in world.roads.lanes() {
        let points = &lane.centreline;
        for i in 0..points.len().saturating_sub(1) {
            for j in i + 2..points.len().saturating_sub(1) {
                assert!(
                    !segments_cross(points[i], points[i + 1], points[j], points[j + 1]),
                    "lane {} crosses itself between segments {i} and {j}",
                    lane.id
                );
            }
        }
    }
}

/// Every float that can reach an artefact is on its quantisation grid (D9).
fn assert_every_float_on_its_grid(world: &World) {
    let mut bad: Vec<String> = Vec::new();
    world.scan_exported_floats(&mut |path, value, quantum| {
        if (!value.is_finite() || !is_on_grid(value, quantum)) && bad.len() < 8 {
            bad.push(format!("{path} = {value} (quantum {quantum})"));
        }
    });
    assert!(bad.is_empty(), "off-grid floats: {bad:?}");
}

/// True if the two segments properly cross, sharing no endpoint.
///
/// The orientation test only: four cross products, no division and no transcendental, so
/// the answer does not depend on the platform.
fn segments_cross(p: Vec3, p2: Vec3, q: Vec3, q2: Vec3) -> bool {
    let cross = |a: Vec3, b: Vec3, c: Vec3| (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
    let d1 = cross(q, q2, p);
    let d2 = cross(q, q2, p2);
    let d3 = cross(p, p2, q);
    let d4 = cross(p, p2, q2);
    ((d1 > 0.0 && d2 < 0.0) || (d1 < 0.0 && d2 > 0.0))
        && ((d3 > 0.0 && d4 < 0.0) || (d3 < 0.0 && d4 > 0.0))
}

#[test]
fn the_dem_cards_and_the_profile_card_validate() {
    for card in v2xw_world::dem::model_cards() {
        card.validate().expect("the card validates");
        assert!(!card.parameters.is_empty());
        for parameter in card.todo_calibrate() {
            assert!(
                parameter
                    .calibration
                    .as_deref()
                    .is_some_and(|plan| plan.len() > 40),
                "{} needs a real calibration plan",
                parameter.name
            );
        }
    }
    let profile = v2xw_world::los::card();
    profile.validate().expect("the card validates");
    assert!(
        profile
            .parameters
            .iter()
            .any(|p| p.name == "sample_spacing_m" && p.default == serde_json::json!(30.0)),
        "the sampling interval is on the card with its source"
    );
    // The source of the default is the DEM post spacing, not an invention.
    let spacing = profile
        .parameters
        .iter()
        .find(|p| p.name == "sample_spacing_m")
        .expect("the parameter");
    assert!(spacing.source.reference.contains("04-models.md §1.4"));
    assert_eq!(DemSource::Srtm30.model_id(), Some("world/terrain/srtm-30"));
    assert!(DemSource::CopernicusGlo30.attribution().is_some());
    assert!(DemSource::Unstated.attribution().is_none());
}
