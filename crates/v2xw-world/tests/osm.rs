//! Tests for `world/source/osm`, over the public API only.
//!
//! Every fixture but the last is a hand-written OSM document of a handful of elements, so
//! that the expected lane, connection and height outcome can be worked out on paper and
//! asserted exactly. The last is the real Manhattan extract (D7), which is `#[ignore]`d
//! because it is 30 MB and lives outside the repository.
//!
//! Fixture geometry uses degrees of about `1e-3`, which at the equator is roughly 111 m —
//! a city block. Latitude 0 is used on purpose: the projection's scale factors are then
//! round numbers and a mistake in the geometry shows up as an obviously wrong metre count
//! rather than as a plausible one.

use std::collections::{BTreeSet, VecDeque};

use v2xw_core::geom::Vec3;
use v2xw_core::ids::LaneId;
use v2xw_world::osm::{
    Anomaly, BboxClip, HighwayPreset, ImportReport, OsmLayers, OsmOptions, OsmSimplifications,
    OsmSource, import_osm, import_osm_bytes, road_junction_ids,
};
use v2xw_world::quant::is_on_grid;
use v2xw_world::{
    ClassMask, HeightSource, JunctionControl, LaneKind, TurnDirection, World, WorldSource,
    WorldSourceSpec, serde_native, serde_vwp,
};

/// The options every test uses: a fixed import date, so nothing reads a clock, and an
/// explicitly named speed preset, because there is no default one (V4/W1).
fn opts() -> OsmOptions {
    OsmOptions::default()
        .imported_at("2026-09-18T00:00:00Z")
        .highway_preset(HighwayPreset::SumoGerman)
}

/// Imports a fixture, failing the test with the report if it does not build.
fn import(xml: &str) -> (World, ImportReport) {
    import_osm_bytes(xml.as_bytes(), "fixture", &opts()).expect("the fixture imports")
}

/// Imports a fixture with adjusted options.
fn import_with(xml: &str, options: &OsmOptions) -> (World, ImportReport) {
    import_osm_bytes(xml.as_bytes(), "fixture", options).expect("the fixture imports")
}

/// Wraps elements in an `<osm>` document.
fn document(body: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <osm version=\"0.6\" generator=\"test\">\n\
         <bounds minlat=\"-0.002\" minlon=\"-0.002\" maxlat=\"0.002\" maxlon=\"0.002\"/>\n\
         {body}\n</osm>\n"
    )
}

/// One `<node>`.
fn node(id: i64, lat: f64, lon: f64) -> String {
    format!("<node id=\"{id}\" lat=\"{lat:.7}\" lon=\"{lon:.7}\"/>\n")
}

/// One `<node>` with tags.
fn tagged_node(id: i64, lat: f64, lon: f64, tags: &[(&str, &str)]) -> String {
    let inner: String = tags
        .iter()
        .map(|(k, v)| format!("  <tag k=\"{k}\" v=\"{v}\"/>\n"))
        .collect();
    format!("<node id=\"{id}\" lat=\"{lat:.7}\" lon=\"{lon:.7}\">\n{inner}</node>\n")
}

/// One `<way>`.
fn way(id: i64, nodes: &[i64], tags: &[(&str, &str)]) -> String {
    let refs: String = nodes
        .iter()
        .map(|n| format!("  <nd ref=\"{n}\"/>\n"))
        .collect();
    let inner: String = tags
        .iter()
        .map(|(k, v)| format!("  <tag k=\"{k}\" v=\"{v}\"/>\n"))
        .collect();
    format!("<way id=\"{id}\">\n{refs}{inner}</way>\n")
}

/// One `<relation>`; members are `(type, ref, role)`.
fn relation(id: i64, members: &[(&str, i64, &str)], tags: &[(&str, &str)]) -> String {
    let inner_members: String = members
        .iter()
        .map(|(kind, r, role)| format!("  <member type=\"{kind}\" ref=\"{r}\" role=\"{role}\"/>\n"))
        .collect();
    let inner: String = tags
        .iter()
        .map(|(k, v)| format!("  <tag k=\"{k}\" v=\"{v}\"/>\n"))
        .collect();
    format!("<relation id=\"{id}\">\n{inner_members}{inner}</relation>\n")
}

/// A plain crossroads: node 2 at the centre, one arm in each direction.
fn crossroads_nodes() -> String {
    [
        node(1, 0.0, -0.001),
        node(2, 0.0, 0.0),
        node(3, 0.0, 0.001),
        node(4, -0.001, 0.0),
        node(5, 0.001, 0.0),
    ]
    .concat()
}

// ---------------------------------------------------------------------------
// One-way handling
// ---------------------------------------------------------------------------

/// A one-way pair: two parallel streets, one east-bound and one west-bound, exactly as
/// Manhattan maps its avenues. 802 of its roughly 940 drivable ways are one-way, so this
/// is the case that matters most.
///
/// Each way has `lanes=2`, so each becomes one edge of two lanes running in the way's own
/// direction and **nothing** in the other: four road lanes in total, and every lane of the
/// east-bound street heads east.
#[test]
fn a_one_way_pair_produces_lanes_in_one_direction_only() {
    let xml = document(
        &[
            node(1, 0.0, -0.001),
            node(2, 0.0, 0.001),
            node(3, 0.0005, -0.001),
            node(4, 0.0005, 0.001),
            way(
                10,
                &[1, 2],
                &[
                    ("highway", "secondary"),
                    ("oneway", "yes"),
                    ("lanes", "2"),
                    ("name", "East Street"),
                ],
            ),
            way(
                11,
                &[4, 3],
                &[
                    ("highway", "secondary"),
                    ("oneway", "yes"),
                    ("lanes", "2"),
                    ("name", "West Street"),
                ],
            ),
        ]
        .concat(),
    );
    let (world, report) = import(&xml);

    // Two disconnected streets, two junctions each, one edge each, two lanes each.
    assert_eq!(report.counts.drivable_ways, 2);
    assert_eq!(world.counts().junctions, 4);
    let road_lanes: Vec<_> = world
        .roads
        .lanes()
        .iter()
        .filter(|l| l.kind == LaneKind::Driving)
        .collect();
    assert_eq!(
        road_lanes.len(),
        4,
        "two lanes on each of two one-way streets"
    );
    assert_eq!(world.roads.edges().len(), 2, "one edge per one-way street");

    // Way 10 runs west to east, so both its lanes head east (heading 0 in ENU).
    let east: Vec<_> = road_lanes
        .iter()
        .filter(|l| l.heading_at(0.0).abs() < 1e-6)
        .collect();
    assert_eq!(east.len(), 2);
    // Way 11 runs east to west: heading is +/- pi.
    let west: Vec<_> = road_lanes
        .iter()
        .filter(|l| (l.heading_at(0.0).abs() - core::f64::consts::PI).abs() < 1e-6)
        .collect();
    assert_eq!(west.len(), 2);

    // Lane 0 is the rightmost in the direction of travel: for the east-bound street that
    // is the southern lane, half a lane width south of the centreline.
    let mut east_sorted: Vec<_> = east.iter().collect();
    east_sorted.sort_by_key(|l| l.index);
    assert!(
        east_sorted[0].start().y < east_sorted[1].start().y,
        "lane 0 is to the right of lane 1 in right-hand traffic"
    );
}

/// `oneway=-1` reverses the way: the lanes run against the node order.
#[test]
fn oneway_minus_one_reverses_the_direction_of_travel() {
    let xml = document(
        &[
            node(1, 0.0, -0.001),
            node(2, 0.0, 0.001),
            way(
                10,
                &[1, 2],
                &[("highway", "residential"), ("oneway", "-1"), ("lanes", "1")],
            ),
        ]
        .concat(),
    );
    let (world, _) = import(&xml);
    let lane = world
        .roads
        .lanes()
        .iter()
        .find(|l| l.kind == LaneKind::Driving)
        .expect("one driving lane");
    assert!(
        (lane.heading_at(0.0).abs() - core::f64::consts::PI).abs() < 1e-6,
        "an oneway=-1 way heads west when its nodes run east"
    );
}

/// A two-way street tagged `lanes=1` still gets a lane each way: the alternative is to
/// turn a residential street one-way by accident.
#[test]
fn a_two_way_street_always_has_a_lane_each_way() {
    let xml = document(
        &[
            node(1, 0.0, -0.001),
            node(2, 0.0, 0.001),
            way(10, &[1, 2], &[("highway", "residential"), ("lanes", "1")]),
        ]
        .concat(),
    );
    let (world, _) = import(&xml);
    let driving = world
        .roads
        .lanes()
        .iter()
        .filter(|l| l.kind == LaneKind::Driving)
        .count();
    assert_eq!(driving, 2);
    assert_eq!(
        world
            .roads
            .edges()
            .iter()
            .filter(|e| !e.is_internal())
            .count(),
        2
    );
}

/// An odd `lanes` count on a two-way street gives the extra lane to the forward direction
/// and says so in the report.
#[test]
fn an_odd_lane_count_is_split_and_reported() {
    let xml = document(
        &[
            node(1, 0.0, -0.001),
            node(2, 0.0, 0.001),
            way(10, &[1, 2], &[("highway", "primary"), ("lanes", "3")]),
        ]
        .concat(),
    );
    let (world, report) = import(&xml);
    assert_eq!(report.anomaly(Anomaly::OddLaneSplit), 1);
    let mut widths: Vec<usize> = world
        .roads
        .edges()
        .iter()
        .filter(|e| !e.is_internal())
        .map(|e| e.lanes.len())
        .collect();
    widths.sort_unstable();
    assert_eq!(widths, vec![1, 2]);
}

// ---------------------------------------------------------------------------
// Junctions and turns
// ---------------------------------------------------------------------------

/// A four-way junction with `turn:lanes` on the approach.
///
/// The approach from the west has two lanes tagged `left|right`: OSM writes the leftmost
/// lane first, so the **leftmost** lane may only turn left (to the north) and the
/// **rightmost** only right (to the south). Nothing may go straight on, because nothing is
/// tagged `through` — and that is exactly what the importer must produce, rather than
/// quietly adding the straight movement the geometry offers.
#[test]
fn turn_lanes_decide_which_lane_makes_which_movement() {
    let xml = document(
        &[
            crossroads_nodes(),
            way(
                10,
                &[1, 2],
                &[
                    ("highway", "primary"),
                    ("oneway", "yes"),
                    ("lanes", "2"),
                    ("turn:lanes", "left|right"),
                ],
            ),
            way(
                11,
                &[2, 3],
                &[("highway", "primary"), ("oneway", "yes"), ("lanes", "2")],
            ),
            way(
                12,
                &[2, 5],
                &[("highway", "primary"), ("oneway", "yes"), ("lanes", "1")],
            ),
            way(
                13,
                &[2, 4],
                &[("highway", "primary"), ("oneway", "yes"), ("lanes", "1")],
            ),
        ]
        .concat(),
    );
    let (world, _) = import(&xml);

    // Node 2 is the only junction with movements through it.
    let junction = world
        .roads
        .junctions()
        .iter()
        .find(|j| !j.internal.is_empty() && j.incoming.len() == 2)
        .expect("the crossroads");
    assert_eq!(
        junction.internal.len(),
        2,
        "one movement per tagged lane, and no straight-on movement"
    );

    let movements: Vec<_> = world
        .roads
        .connections()
        .iter()
        .filter(|c| c.via.is_some())
        .collect();
    assert_eq!(movements.len(), 2);
    let mut directions: Vec<TurnDirection> = movements.iter().map(|c| c.direction).collect();
    directions.sort();
    assert_eq!(directions, vec![TurnDirection::Left, TurnDirection::Right]);

    // The left turn leaves the leftmost lane (index 1) and the right turn the rightmost.
    for movement in &movements {
        let from = world.lane(movement.from_lane);
        match movement.direction {
            TurnDirection::Left => assert_eq!(from.index, 1),
            TurnDirection::Right => assert_eq!(from.index, 0),
            other => panic!("unexpected direction {other:?}"),
        }
    }

    // Nothing reaches the eastern street, because no lane is tagged `through`.
    let east_edge = world
        .roads
        .edges()
        .iter()
        .find(|e| !e.is_internal() && world.lane(e.lanes[0]).heading_at(0.0).abs() < 1e-6)
        .expect("the eastern departure");
    assert!(
        !world
            .roads
            .connections()
            .iter()
            .any(|c| east_edge.lanes.contains(&c.to_lane)),
        "a `left|right` tag bans going straight on"
    );
}

/// Without `turn:lanes` the movements are inferred geometrically: right from the rightmost
/// lane, left from the leftmost, straight from every lane.
#[test]
fn turns_are_inferred_geometrically_when_untagged() {
    let xml = document(
        &[
            crossroads_nodes(),
            way(
                10,
                &[1, 2],
                &[("highway", "primary"), ("oneway", "yes"), ("lanes", "2")],
            ),
            way(
                11,
                &[2, 3],
                &[("highway", "primary"), ("oneway", "yes"), ("lanes", "2")],
            ),
            way(
                12,
                &[2, 5],
                &[("highway", "primary"), ("oneway", "yes"), ("lanes", "1")],
            ),
            way(
                13,
                &[2, 4],
                &[("highway", "primary"), ("oneway", "yes"), ("lanes", "1")],
            ),
        ]
        .concat(),
    );
    let (world, _) = import(&xml);
    let movements: Vec<_> = world
        .roads
        .connections()
        .iter()
        .filter(|c| c.via.is_some())
        .collect();
    // lane 0: right and straight; lane 1: straight and left. Four movements.
    assert_eq!(movements.len(), 4);
    let mut by_lane: Vec<(u8, TurnDirection)> = movements
        .iter()
        .map(|c| (world.lane(c.from_lane).index, c.direction))
        .collect();
    by_lane.sort();
    assert_eq!(
        by_lane,
        vec![
            (0, TurnDirection::Straight),
            (0, TurnDirection::Right),
            (1, TurnDirection::Straight),
            (1, TurnDirection::Left),
        ]
    );
    // Every movement's connector is an internal lane of the crossroads.
    for movement in &movements {
        let via = world.lane(movement.via.expect("a connector"));
        assert_eq!(via.kind, LaneKind::Internal);
        assert!(via.junction.is_some());
    }
}

/// A `no_left_turn` restriction relation bans the movement it names and leaves the rest.
#[test]
fn a_turn_restriction_bans_exactly_one_movement() {
    let body = [
        crossroads_nodes(),
        way(
            10,
            &[1, 2],
            &[("highway", "primary"), ("oneway", "yes"), ("lanes", "2")],
        ),
        way(
            11,
            &[2, 3],
            &[("highway", "primary"), ("oneway", "yes"), ("lanes", "2")],
        ),
        way(
            12,
            &[2, 5],
            &[("highway", "primary"), ("oneway", "yes"), ("lanes", "1")],
        ),
        relation(
            20,
            &[("way", 10, "from"), ("node", 2, "via"), ("way", 12, "to")],
            &[("type", "restriction"), ("restriction", "no_left_turn")],
        ),
    ]
    .concat();
    let (world, report) = import(&document(&body));
    assert_eq!(report.counts.restrictions_applied, 1);
    let banned: Vec<_> = world
        .roads
        .connections()
        .iter()
        .filter(|c| !c.permitted)
        .collect();
    assert!(!banned.is_empty(), "the restriction bans something");
    for connection in &banned {
        assert_eq!(connection.direction, TurnDirection::Left);
    }
    assert!(
        world.roads.connections().iter().any(|c| c.permitted),
        "the other movements survive"
    );
}

/// A roundabout: the ring is one-way and every junction on it is controlled as a
/// roundabout.
#[test]
fn a_roundabout_is_one_way_and_controlled_as_one() {
    let body = [
        node(1, 0.0002, 0.0),
        node(2, 0.0, 0.0002),
        node(3, -0.0002, 0.0),
        node(4, 0.0, -0.0002),
        node(5, 0.0, -0.002),
        node(6, 0.0, 0.002),
        way(
            10,
            &[1, 2, 3, 4, 1],
            &[
                ("highway", "tertiary"),
                ("junction", "roundabout"),
                ("lanes", "1"),
            ],
        ),
        way(11, &[5, 4], &[("highway", "tertiary"), ("lanes", "1")]),
        way(12, &[2, 6], &[("highway", "tertiary"), ("lanes", "1")]),
    ]
    .concat();
    let (world, _) = import(&document(&body));

    let ring_junctions: Vec<_> = world
        .roads
        .junctions()
        .iter()
        .filter(|j| j.control == JunctionControl::Roundabout)
        .collect();
    assert!(
        ring_junctions.len() >= 2,
        "the arms meet the ring at roundabout junctions, found {}",
        ring_junctions.len()
    );
    // A roundabout way is implicitly one-way: no pair of ring edges runs both ways between
    // the same two junctions.
    for edge in world.roads.edges() {
        if edge.is_internal() {
            continue;
        }
        let reverse = world
            .roads
            .edges()
            .iter()
            .filter(|other| other.from == edge.to && other.to == edge.from)
            .count();
        assert!(reverse <= 1);
    }
}

/// The trivial two-road junction collapse: one street mapped as two ways with identical
/// tags becomes one edge per direction and the node between them stops being a junction.
#[test]
fn a_trivial_two_road_junction_is_collapsed() {
    let body = [
        node(1, 0.0, -0.001),
        node(2, 0.0, 0.0),
        node(3, 0.0, 0.001),
        way(
            10,
            &[1, 2],
            &[("highway", "residential"), ("lanes", "2"), ("name", "Main")],
        ),
        way(
            11,
            &[2, 3],
            &[("highway", "residential"), ("lanes", "2"), ("name", "Main")],
        ),
    ]
    .concat();
    let xml = document(&body);

    let (collapsed, report) = import(&xml);
    assert_eq!(report.counts.segments_before_collapse, 2);
    assert_eq!(report.counts.segments_after_collapse, 1);
    assert_eq!(report.counts.junctions_collapsed, 1);
    assert_eq!(collapsed.counts().junctions, 2, "only the two ends remain");
    // The surviving street lane runs the whole length, through the old junction.
    let longest = collapsed
        .roads
        .lanes()
        .iter()
        .filter(|l| l.kind == LaneKind::Driving)
        .map(|l| l.length_m)
        .fold(0.0f64, f64::max);
    assert!(longest > 200.0, "the two blocks merged, got {longest} m");

    // With the simplification off, the middle node stays a junction.
    let mut options = opts();
    options.simplify = OsmSimplifications {
        collapse_trivial_junctions: false,
        ..OsmSimplifications::default()
    };
    let (kept, report) = import_with(&xml, &options);
    assert_eq!(report.counts.junctions_collapsed, 0);
    assert_eq!(kept.counts().junctions, 3);
}

/// A street whose two halves differ — a name change — is not collapsed: the junction is
/// where the attributes change, and merging would lose the name.
#[test]
fn a_junction_between_different_streets_is_kept() {
    let body = [
        node(1, 0.0, -0.001),
        node(2, 0.0, 0.0),
        node(3, 0.0, 0.001),
        way(
            10,
            &[1, 2],
            &[("highway", "residential"), ("lanes", "2"), ("name", "Main")],
        ),
        way(
            11,
            &[2, 3],
            &[
                ("highway", "residential"),
                ("lanes", "2"),
                ("name", "Other"),
            ],
        ),
    ]
    .concat();
    let (world, report) = import(&document(&body));
    assert_eq!(report.counts.junctions_collapsed, 0);
    assert_eq!(world.counts().junctions, 3);
}

// ---------------------------------------------------------------------------
// Signals
// ---------------------------------------------------------------------------

/// A `traffic_signals` node on a junction produces a fixed-time plan whose phases fill its
/// cycle and whose amber comes from the approach speed.
#[test]
fn a_traffic_signals_node_synthesises_a_plan() {
    let body = [
        node(1, 0.0, -0.001),
        tagged_node(2, 0.0, 0.0, &[("highway", "traffic_signals")]),
        node(3, 0.0, 0.001),
        node(4, -0.001, 0.0),
        node(5, 0.001, 0.0),
        way(
            10,
            &[1, 2, 3],
            &[
                ("highway", "primary"),
                ("lanes", "2"),
                ("maxspeed", "30 mph"),
            ],
        ),
        way(
            11,
            &[4, 2, 5],
            &[
                ("highway", "primary"),
                ("lanes", "2"),
                ("maxspeed", "30 mph"),
            ],
        ),
    ]
    .concat();
    let (world, report) = import(&document(&body));
    assert_eq!(report.counts.traffic_signal_nodes, 1);
    assert_eq!(report.counts.signalised_junctions, 1);

    let plan = &world.signals[0];
    let junction = world.junction(plan.junction);
    assert_eq!(
        junction.control,
        JunctionControl::Signalised { plan: plan.id }
    );
    assert_eq!(plan.controlled.len(), junction.internal.len());
    for phase in &plan.phases {
        assert_eq!(phase.states.len(), plan.controlled.len());
    }
    // Phases fill the cycle exactly, which `World::validate` requires.
    assert!((plan.total_phase_duration_s() - plan.cycle_s).abs() <= 1e-3);
    // Two groups, each with a green and an amber, and no all-red by default.
    assert_eq!(plan.phases.len(), 4);
    // 30 mph is 13.4112 m/s, so y = 1 + 13.4112 / 6 = 3.235 s, rounded to 3.2 s.
    let amber = plan.phases[1].duration_s;
    assert!(
        (amber - 3.2).abs() < 1e-9,
        "amber from the ITE formula, got {amber}"
    );
    assert!(world.validate().is_ok());
}

/// A signal node that is not itself a junction is attached to the nearest junction within
/// the guess distance, and reported as an orphan when there is none.
#[test]
fn an_off_junction_signal_node_is_guessed_onto_the_nearest_junction() {
    let body = [
        node(1, 0.0, -0.001),
        node(2, 0.0, 0.0),
        node(3, 0.0, 0.001),
        node(4, -0.001, 0.0),
        // 0.00005 degrees north of the junction is about 5.5 m: inside the 25 m default.
        tagged_node(6, 0.00005, 0.0, &[("highway", "traffic_signals")]),
        // Far away from anything.
        tagged_node(7, 0.0018, 0.0018, &[("highway", "traffic_signals")]),
        way(10, &[1, 2, 3], &[("highway", "primary"), ("lanes", "2")]),
        way(11, &[4, 2], &[("highway", "primary"), ("lanes", "2")]),
    ]
    .concat();
    let (world, report) = import(&document(&body));
    assert_eq!(report.counts.traffic_signal_nodes, 2);
    assert_eq!(report.counts.signalised_junctions, 1);
    assert_eq!(report.anomaly(Anomaly::OrphanTrafficSignal), 1);
    assert_eq!(world.signals.len(), 1);
}

// ---------------------------------------------------------------------------
// Buildings
// ---------------------------------------------------------------------------

/// A multipolygon building with a courtyard: one building, one hole, and the height taken
/// from `building:levels` because there is no `height` tag.
#[test]
fn a_multipolygon_building_keeps_its_hole_and_its_levels_height() {
    let body = [
        // Outer ring, about 110 m square.
        node(1, 0.0, 0.0),
        node(2, 0.0, 0.001),
        node(3, 0.001, 0.001),
        node(4, 0.001, 0.0),
        // Inner ring, a courtyard in the middle.
        node(5, 0.0003, 0.0003),
        node(6, 0.0003, 0.0007),
        node(7, 0.0007, 0.0007),
        node(8, 0.0007, 0.0003),
        way(10, &[1, 2, 3, 4, 1], &[]),
        way(11, &[5, 6, 7, 8, 5], &[]),
        relation(
            20,
            &[("way", 10, "outer"), ("way", 11, "inner")],
            &[
                ("type", "multipolygon"),
                ("building", "yes"),
                ("building:levels", "7"),
                ("building:material", "brick"),
                ("name", "Courtyard Block"),
            ],
        ),
    ]
    .concat();
    let (world, report) = import(&document(&body));
    assert_eq!(report.counts.buildings, 1);
    assert_eq!(report.counts.building_holes, 1);
    assert_eq!(report.counts.heights_from_levels, 1);

    let building = &world.buildings[0];
    assert_eq!(building.holes.len(), 1);
    assert_eq!(building.levels, Some(7));
    assert_eq!(building.height_source, HeightSource::FromLevels);
    // 7 storeys at the default 3 m per storey, which is `TODO: calibrate`.
    assert!((building.height_m - 21.0).abs() < 1e-9);
    assert_eq!(building.material, v2xw_world::MaterialClass::Brick);
    // The outer ring is counter-clockwise and closed; the hole is wound the other way.
    assert_eq!(building.footprint.first(), building.footprint.last());
    assert!(v2xw_world::ring_signed_area_2x(&building.footprint) > 0.0);
    assert!(v2xw_world::ring_signed_area_2x(&building.holes[0]) < 0.0);
    // A point in the courtyard is not inside the building.
    let centre = building.footprint[0].lerp(building.footprint[2], 0.5);
    assert!(!building.contains_2d(centre));
}

/// The height rules, in order: an explicit `height` wins, then levels, then the default —
/// and each building records which rule fired.
#[test]
fn the_height_rule_that_fired_is_recorded_per_building() {
    let square = |base: i64, lat: f64, lon: f64| {
        [
            node(base, lat, lon),
            node(base + 1, lat, lon + 0.0002),
            node(base + 2, lat + 0.0002, lon + 0.0002),
            node(base + 3, lat + 0.0002, lon),
        ]
        .concat()
    };
    let body = [
        square(1, 0.0, 0.0),
        square(11, 0.0, 0.001),
        square(21, 0.0, 0.002),
        way(
            100,
            &[1, 2, 3, 4, 1],
            &[
                ("building", "yes"),
                ("height", "12.5 m"),
                ("building:levels", "4"),
            ],
        ),
        way(
            101,
            &[11, 12, 13, 14, 11],
            &[("building", "yes"), ("building:levels", "4")],
        ),
        way(102, &[21, 22, 23, 24, 21], &[("building", "yes")]),
    ]
    .concat();
    let (world, report) = import(&document(&body));
    assert_eq!(report.counts.buildings, 3);
    assert_eq!(report.counts.heights_tagged, 1);
    assert_eq!(report.counts.heights_from_levels, 1);
    assert_eq!(report.counts.heights_defaulted, 1);

    let by_source = |source: HeightSource| {
        world
            .buildings
            .iter()
            .find(|b| b.height_source == source)
            .expect("a building with that height source")
    };
    assert!((by_source(HeightSource::Tagged).height_m - 12.5).abs() < 1e-9);
    assert!((by_source(HeightSource::FromLevels).height_m - 12.0).abs() < 1e-9);
    assert!((by_source(HeightSource::Defaulted).height_m - 10.0).abs() < 1e-9);
    // An explicit height still records the levels it was tagged with.
    assert_eq!(by_source(HeightSource::Tagged).levels, Some(4));
}

// ---------------------------------------------------------------------------
// Robustness
// ---------------------------------------------------------------------------

/// A way referencing a node the extract does not carry: counted, not fatal, and the rest
/// of the way is still imported.
#[test]
fn a_missing_node_reference_is_counted_not_fatal() {
    let body = [
        node(1, 0.0, -0.001),
        node(2, 0.0, 0.0),
        node(3, 0.0, 0.001),
        // Node 99 is never declared.
        way(10, &[1, 99, 2, 3], &[("highway", "residential")]),
        // A way with one resolvable node is too short to be anything.
        way(11, &[3, 98], &[("highway", "residential")]),
    ]
    .concat();
    let (world, report) = import(&document(&body));
    assert_eq!(report.anomaly(Anomaly::MissingNode), 2);
    assert_eq!(report.anomaly(Anomaly::WayTooShort), 1);
    assert!(world.counts().lanes > 0, "the good way still imports");
    assert!(world.validate().is_ok());
}

/// Absurd tag values are parsed as far as they can be and then counted.
#[test]
fn absurd_tag_values_are_counted_and_defaulted() {
    let body = [
        node(1, 0.0, -0.001),
        node(2, 0.0, 0.001),
        node(3, 0.0005, 0.0),
        node(4, 0.0005, 0.0002),
        node(5, 0.0007, 0.0002),
        node(6, 0.0007, 0.0),
        way(
            10,
            &[1, 2],
            &[
                ("highway", "residential"),
                ("maxspeed", "signals"),
                ("oneway", "sometimes"),
                ("lanes", "two"),
            ],
        ),
        way(
            11,
            &[3, 4, 5, 6, 3],
            &[
                ("building", "yes"),
                ("height", "12;15"),
                ("building:levels", "x"),
            ],
        ),
    ]
    .concat();
    let (world, report) = import(&document(&body));
    assert_eq!(report.anomaly(Anomaly::UnparsableMaxspeed), 1);
    assert_eq!(report.anomaly(Anomaly::UnparsableOneway), 1);
    assert_eq!(report.anomaly(Anomaly::UnparsableLanes), 1);
    assert_eq!(report.anomaly(Anomaly::MultiValuedHeight), 1);
    assert_eq!(report.anomaly(Anomaly::UnparsableLevels), 1);

    // `maxspeed=signals` falls back to the residential class default of 13.89 m/s.
    let lane = world
        .roads
        .lanes()
        .iter()
        .find(|l| l.kind == LaneKind::Driving)
        .expect("a driving lane");
    assert!((lane.speed_limit_mps - 13.89).abs() < 1e-9);
    // `height=12;15` takes the first value.
    assert!((world.buildings[0].height_m - 12.0).abs() < 1e-9);
    // The unreadable `oneway` falls back to a two-way street.
    assert_eq!(
        world
            .roads
            .edges()
            .iter()
            .filter(|e| !e.is_internal())
            .count(),
        2
    );
}

/// A degenerate document — a way with one node, a zero-length way, an unknown highway
/// value — produces a world rather than a panic.
#[test]
fn degenerate_input_does_not_panic() {
    let body = [
        node(1, 0.0, 0.0),
        node(2, 0.0, 0.0),
        node(3, 0.0, 0.001),
        way(10, &[1], &[("highway", "residential")]),
        way(11, &[1, 2], &[("highway", "residential")]),
        way(12, &[1, 3], &[("highway", "proposed")]),
        way(13, &[], &[("highway", "residential")]),
        relation(20, &[], &[("type", "multipolygon"), ("building", "yes")]),
    ]
    .concat();
    let (world, report) = import(&document(&body));
    assert!(report.anomaly(Anomaly::WayTooShort) >= 2);
    assert_eq!(report.anomaly(Anomaly::UnknownHighwayValue), 1);
    assert_eq!(report.anomaly(Anomaly::NoOuterRing), 1);
    assert!(world.validate().is_ok());
}

// ---------------------------------------------------------------------------
// Layers and access
// ---------------------------------------------------------------------------

/// Pedestrian and cycle ways join the same lane graph and are never routable by a vehicle.
#[test]
fn soft_lanes_are_never_open_to_motor_traffic() {
    let body = [
        node(1, 0.0, -0.001),
        node(2, 0.0, 0.001),
        node(3, 0.0002, -0.001),
        node(4, 0.0002, 0.001),
        node(5, 0.0004, -0.001),
        node(6, 0.0004, 0.001),
        way(10, &[1, 2], &[("highway", "residential")]),
        way(11, &[3, 4], &[("highway", "footway")]),
        way(12, &[5, 6], &[("highway", "cycleway")]),
    ]
    .concat();
    let (world, report) = import(&document(&body));
    assert_eq!(report.counts.pedestrian_ways, 1);
    assert_eq!(report.counts.cycle_ways, 1);
    assert!(report.counts.sidewalk_lanes > 0);
    assert!(report.counts.cycle_lanes > 0);

    for lane in world.roads.lanes() {
        match lane.kind {
            LaneKind::Sidewalk | LaneKind::Cycle => assert!(
                !lane.admits(ClassMask::MOTOR_TRAFFIC),
                "lane {} of kind {:?} admits motor traffic",
                lane.id,
                lane.kind
            ),
            _ => {}
        }
    }
}

/// `access=private` on a service road removes it from the network.
#[test]
fn a_private_road_is_dropped_and_counted() {
    let body = [
        node(1, 0.0, -0.001),
        node(2, 0.0, 0.001),
        way(
            10,
            &[1, 2],
            &[("highway", "service"), ("access", "private")],
        ),
    ]
    .concat();
    let (world, report) = import(&document(&body));
    assert_eq!(report.anomaly(Anomaly::AccessDenied), 1);
    assert_eq!(world.counts().lanes, 0);
}

/// `sidewalk=both` materialises a pavement each side when the option is on, and nothing
/// when it is off — the default, because a city that maps pavements as separate `footway`
/// ways would otherwise get two for every one on the ground.
#[test]
fn sidewalk_tags_become_lanes_only_when_asked_for() {
    let body = [
        node(1, 0.0, -0.001),
        node(2, 0.0, 0.001),
        way(
            10,
            &[1, 2],
            &[
                ("highway", "residential"),
                ("lanes", "2"),
                ("sidewalk", "both"),
            ],
        ),
    ]
    .concat();
    let xml = document(&body);

    let (without, report) = import(&xml);
    assert_eq!(report.counts.sidewalk_lanes, 0);
    assert!(
        without
            .roads
            .lanes()
            .iter()
            .all(|l| l.kind != LaneKind::Sidewalk)
    );

    let mut options = opts();
    options.sidewalks_from_tags = true;
    let (with, report) = import_with(&xml, &options);
    // Two sides, one lane each way on each: four pavement lanes.
    assert_eq!(report.counts.sidewalk_lanes, 4);
    let pavements: Vec<_> = with
        .roads
        .lanes()
        .iter()
        .filter(|l| l.kind == LaneKind::Sidewalk)
        .collect();
    assert_eq!(pavements.len(), 4);
    // They sit outside the carriageway: |y| is at least half the carriageway width.
    let carriageway_half = 3.5;
    let centre_y = with
        .roads
        .lanes()
        .iter()
        .filter(|l| l.kind == LaneKind::Driving)
        .map(|l| l.start().y)
        .sum::<f64>()
        / 2.0;
    for pavement in &pavements {
        assert!(
            (pavement.start().y - centre_y).abs() > carriageway_half,
            "a pavement at {} is inside the carriageway centred on {centre_y}",
            pavement.start().y
        );
        assert!(!pavement.admits(ClassMask::MOTOR_TRAFFIC));
    }
}

/// Turning a layer off keeps it out of the world.
#[test]
fn layers_can_be_switched_off() {
    let body = [
        node(1, 0.0, -0.001),
        node(2, 0.0, 0.001),
        node(3, 0.0002, -0.001),
        node(4, 0.0002, 0.001),
        way(10, &[1, 2], &[("highway", "residential")]),
        way(11, &[3, 4], &[("highway", "footway")]),
    ]
    .concat();
    let mut options = opts();
    options.layers = OsmLayers::roads_only();
    let (world, report) = import_with(&document(&body), &options);
    assert_eq!(report.counts.pedestrian_ways, 0);
    assert!(
        world
            .roads
            .lanes()
            .iter()
            .all(|l| l.kind != LaneKind::Sidewalk)
    );
}

// ---------------------------------------------------------------------------
// Determinism, quantisation and the writers
// ---------------------------------------------------------------------------

/// A representative fixture: a signalised crossroads, a footway, a building and a park.
fn mixed_fixture() -> String {
    document(
        &[
            node(1, 0.0, -0.001),
            tagged_node(2, 0.0, 0.0, &[("highway", "traffic_signals")]),
            node(3, 0.0, 0.001),
            node(4, -0.001, 0.0),
            node(5, 0.001, 0.0),
            node(6, 0.0003, -0.001),
            node(7, 0.0003, 0.001),
            node(20, 0.0004, 0.0004),
            node(21, 0.0004, 0.0008),
            node(22, 0.0008, 0.0008),
            node(23, 0.0008, 0.0004),
            node(30, -0.0008, -0.0008),
            node(31, -0.0008, -0.0004),
            node(32, -0.0004, -0.0004),
            node(33, -0.0004, -0.0008),
            way(
                10,
                &[1, 2, 3],
                &[
                    ("highway", "primary"),
                    ("lanes", "4"),
                    ("maxspeed", "40 mph"),
                    ("name", "Broad Street"),
                ],
            ),
            way(
                11,
                &[4, 2, 5],
                &[
                    ("highway", "secondary"),
                    ("lanes", "2"),
                    ("name", "Cross Street"),
                ],
            ),
            way(12, &[6, 7], &[("highway", "footway")]),
            way(
                100,
                &[20, 21, 22, 23, 20],
                &[("building", "yes"), ("height", "31.5")],
            ),
            way(101, &[30, 31, 32, 33, 30], &[("leisure", "park")]),
        ]
        .concat(),
    )
}

/// The same bytes import to the same world, content hash included — conformance item W5.
#[test]
fn the_import_is_deterministic() {
    let xml = mixed_fixture();
    let (a, report_a) = import(&xml);
    let (b, report_b) = import(&xml);
    assert_eq!(a.content_hash, b.content_hash);
    assert_eq!(report_a, report_b);
    assert_eq!(a, b);
}

/// Every float in an imported world sits on its quantisation grid (D9).
#[test]
fn every_exported_float_is_on_its_grid() {
    let (world, _) = import(&mixed_fixture());
    let mut off = Vec::new();
    world.scan_exported_floats(&mut |field, value, quantum| {
        if !is_on_grid(value, quantum) {
            off.push(format!("{field} = {value} (quantum {quantum})"));
        }
    });
    assert!(off.is_empty(), "off-grid values: {off:?}");
}

/// An imported world survives both serialisers and the `vwp-world/1` payload.
#[test]
fn an_imported_world_round_trips_through_both_writers() {
    let (world, _) = import(&mixed_fixture());

    let bytes = serde_native::to_bytes(&world).expect("the native writer accepts it");
    let back = serde_native::from_bytes(&bytes).expect("and reads it back");
    assert_eq!(back.content_hash, world.content_hash);

    let json = serde_native::to_json(&world).expect("json");
    let from_json = serde_native::from_json(&json).expect("json round trip");
    assert_eq!(from_json.content_hash, world.content_hash);

    // The payload carries its own digest — SHA-256 of its body, not the world's geometry
    // hash (docs/protocol/vwp-v1.md §4.1) — so what must agree is the payload with itself.
    let payload = serde_vwp::write(&world).expect("the vwp writer accepts it");
    assert_eq!(
        serde_vwp::verify(&payload.bytes).expect("the payload verifies"),
        payload.content_hash
    );
    assert_eq!(
        serde_vwp::stored_content_hash(&payload.bytes).expect("the stored digest"),
        payload.content_hash
    );
}

/// The provenance records the ODbL, its attribution, and every transformation that ran —
/// including the simplifications that did not (invariant I-W3).
#[test]
fn the_provenance_records_the_licence_and_every_transformation() {
    let (world, report) = import(&mixed_fixture());
    let provenance = &world.provenance;
    assert_eq!(provenance.source, v2xw_world::WorldSourceKind::Osm);
    assert!(provenance.source_id.starts_with("sha256:"));
    assert!(provenance.source_id.contains(&report.source_sha256));
    assert_eq!(provenance.imported_at, "2026-09-18T00:00:00Z");
    assert_eq!(provenance.wire_licence(), "ODbL-1.0");
    assert_eq!(
        provenance.required_attributions(),
        vec!["© OpenStreetMap contributors"]
    );
    let names: BTreeSet<&str> = provenance
        .transformations
        .iter()
        .map(|t| t.name.as_str())
        .collect();
    for wanted in [
        "osm-import",
        "local-tangent-plane",
        "split-ways",
        "collapse-trivial-junctions",
        "lane-layout",
        "junction-trim",
        "turn-inference",
        "signal-guess",
        "height-default",
        "terrain",
        "quantise",
        "not-applied",
    ] {
        assert!(
            names.contains(wanted),
            "no {wanted} transformation recorded"
        );
    }
    assert!(
        report
            .skipped_simplifications
            .contains(&"merge-dog-leg-junctions".to_string()),
        "the unimplemented simplifications are declared"
    );
    // The content hash is written back into the provenance (invariant I-W2).
    assert_eq!(provenance.content_hash, world.content_hash);
}

/// The world's origin is the south-west corner of its own geometry (D6): every coordinate
/// is non-negative.
#[test]
fn the_origin_is_the_south_west_corner() {
    let (world, _) = import(&mixed_fixture());
    assert!(world.bbox.min.x >= 0.0, "min x is {}", world.bbox.min.x);
    assert!(world.bbox.min.y >= 0.0, "min y is {}", world.bbox.min.y);
    for lane in world.roads.lanes() {
        for point in &lane.centreline {
            assert!(point.x >= 0.0 && point.y >= 0.0);
        }
    }
}

/// The model card names every parameter, and the ones with no source are marked
/// `todo-calibrate` with a plan (registry rule R1).
#[test]
fn the_model_card_is_valid_and_declares_its_uncalibrated_parameters() {
    let card = v2xw_world::osm::card();
    card.validate().expect("the card is valid");
    assert_eq!(card.id, "world/source/osm");
    let todo: Vec<&str> = card.todo_calibrate().map(|p| p.name.as_str()).collect();
    for wanted in [
        "metres_per_level",
        "default_height_m",
        "highway_class_defaults",
    ] {
        assert!(
            todo.contains(&wanted),
            "{wanted} is not marked todo-calibrate"
        );
    }
}

/// The `WorldSource` seam builds the same world as the direct call, and refuses a
/// specification it does not implement.
#[test]
fn the_world_source_seam_works() {
    let xml = mixed_fixture();
    let directory = std::env::temp_dir().join("v2xw-osm-source-test");
    std::fs::create_dir_all(&directory).expect("scratch directory");
    let path = directory.join("fixture.osm.xml");
    std::fs::write(&path, &xml).expect("write the fixture");

    // The seam is handed only the common ImportOptions, so the OSM-specific preset comes
    // from the source's own options — and `OsmSource::new()` has none, by design (V4/W1).
    assert!(
        OsmSource::new()
            .build(
                &WorldSourceSpec::OsmXml {
                    path: path.display().to_string(),
                    bbox: None,
                },
                &opts().import,
            )
            .is_err(),
        "a source with no preset must refuse"
    );
    let source = OsmSource::with_options(opts());
    let world = source
        .build(
            &WorldSourceSpec::OsmXml {
                path: path.display().to_string(),
                bbox: None,
            },
            &opts().import,
        )
        .expect("the source builds");
    let (direct, _) = import_osm(&path, &opts()).expect("the direct call builds");
    assert_eq!(world.content_hash, direct.content_hash);

    let refused = source.build(
        &WorldSourceSpec::SumoNet {
            path: "x".to_string(),
        },
        &opts().import,
    );
    assert!(refused.is_err());
    std::fs::remove_file(&path).ok();
}

// ---------------------------------------------------------------------------
// The defects the 2026-09-18 review found (docs/design/findings/world-review-register.md)
// ---------------------------------------------------------------------------

/// A document with no `<bounds>` element, so the frame has to fall back on the geometry.
fn document_without_bounds(body: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <osm version=\"0.6\" generator=\"test\">\n{body}\n</osm>\n"
    )
}

/// True if a polyline crosses itself: any two non-adjacent segments properly intersect.
///
/// Written out here rather than borrowed from the crate, so that the test is an
/// independent check on the importer's own repair rather than a restatement of it.
fn self_intersects(points: &[Vec3]) -> bool {
    let crosses = |p: Vec3, p2: Vec3, q: Vec3, q2: Vec3| {
        let (rx, ry) = (p2.x - p.x, p2.y - p.y);
        let (sx, sy) = (q2.x - q.x, q2.y - q.y);
        let denominator = rx * sy - ry * sx;
        if denominator == 0.0 {
            return false;
        }
        let (qx, qy) = (q.x - p.x, q.y - p.y);
        let t = (qx * sy - qy * sx) / denominator;
        let u = (qx * ry - qy * rx) / denominator;
        t > 0.0 && t < 1.0 && u > 0.0 && u < 1.0
    };
    for i in 0..points.len().saturating_sub(1) {
        for j in i + 2..points.len().saturating_sub(1) {
            if crosses(points[i], points[i + 1], points[j], points[j + 1]) {
                return true;
            }
        }
    }
    false
}

/// V3 (high). The world frame comes from the **requested** box, so geometry that
/// overhangs the request further cannot move the origin — and therefore cannot move every
/// metre coordinate and the content hash with it.
///
/// Before this, the origin was the south-west corner of whatever was kept, so the Phase 1
/// world's origin was set by one FDR Drive lane's last node and one West 48th Street way's
/// first node: 370 m south and 467 m west of the box that was asked for.
#[test]
fn the_frame_comes_from_the_requested_box_not_from_the_overhang() {
    let core = [
        node(1, 0.0, -0.0005),
        node(2, 0.0, 0.0005),
        way(
            10,
            &[1, 2],
            &[("highway", "residential"), ("name", "Main Street")],
        ),
    ]
    .concat();
    // A second way that starts inside the box and runs 440 m to the south-west, so it is
    // kept by the filter and its far node is the geometry's real south-west corner.
    let overhang = [
        node(5, 0.0005, 0.0005),
        node(6, -0.004, -0.004),
        way(11, &[5, 6], &[("highway", "residential")]),
    ]
    .concat();

    let bbox = v2xw_world::GeoBbox::new(-0.001, -0.001, 0.001, 0.001);
    let options = OsmOptions {
        bbox: Some(bbox),
        ..opts()
    };
    let (plain, plain_report) = import_with(&document(&core), &options);
    let (extended, extended_report) = import_with(
        &document(&[core.clone(), overhang.clone()].concat()),
        &options,
    );

    assert_eq!(
        plain.origin, extended.origin,
        "extra overhanging geometry moved the frame"
    );
    assert_eq!(
        plain.origin.lat_deg, -0.001,
        "the requested south-west corner"
    );
    assert_eq!(plain.origin.lon_deg, -0.001);
    assert_eq!(plain_report.frame.label(), "requested-bbox");
    assert_eq!(extended_report.frame.label(), "requested-bbox");
    assert_eq!(plain_report.requested_bbox, Some(bbox));
    // And the frame that was used is on the record, not implied.
    assert!(
        extended
            .provenance
            .transformations
            .iter()
            .any(|t| t.name == "local-tangent-plane"
                && t.to_wire_string().contains("frame=requested-bbox")),
        "the provenance does not name the frame rule"
    );

    // With no request at all the extract's own declared bounds fix the frame, which is
    // still independent of the geometry.
    let (a, report_a) = import(&document(&core));
    let (b, _) = import(&document(&[core.clone(), overhang.clone()].concat()));
    assert_eq!(report_a.frame.label(), "extract-bounds");
    assert_eq!(a.origin, b.origin, "declared bounds fix the frame too");

    // Only with neither does the frame follow the geometry — and the report says so, so
    // the one irreproducible case is never silent.
    let (c, report_c) = import(&document_without_bounds(&core));
    let (d, report_d) = import(&document_without_bounds(&[core, overhang].concat()));
    assert_eq!(report_c.frame.label(), "imported-geometry");
    assert_eq!(report_d.frame.label(), "imported-geometry");
    assert_ne!(
        c.origin, d.origin,
        "with no box and no bounds the frame does follow the geometry"
    );
}

/// V1 (high). A `building=*` outline is one structure and one obstacle; the
/// `building:part=*` volumes inside it are its detail, not two more buildings.
///
/// 59.3 % of the Phase 1 world's 7 390 "buildings" were part fragments, so the count was
/// inflated 2.44×, the obstacle R-tree carried 2.4× the entries it needed, and the tallest
/// building in Midtown was the Empire State Building's spire.
#[test]
fn a_building_part_inside_an_outline_is_not_a_second_building() {
    let square = |base: i64, lat: f64, lon: f64, size: f64| {
        [
            node(base, lat, lon),
            node(base + 1, lat, lon + size),
            node(base + 2, lat + size, lon + size),
            node(base + 3, lat + size, lon),
        ]
        .concat()
    };
    let body = [
        square(1, 0.0, 0.0, 0.002),         // the outline, about 222 m square
        square(11, 0.0002, 0.0002, 0.0004), // a part inside it
        square(21, 0.001, 0.001, 0.0004),   // a second part inside it
        square(31, 0.004, 0.004, 0.0004),   // a part with no outline over it
        way(
            100,
            &[1, 2, 3, 4, 1],
            &[("building", "yes"), ("height", "50"), ("name", "Tower")],
        ),
        way(
            101,
            &[11, 12, 13, 14, 11],
            &[("building:part", "yes"), ("height", "120")],
        ),
        way(
            102,
            &[21, 22, 23, 24, 21],
            &[("building:part", "yes"), ("height", "35")],
        ),
        way(
            103,
            &[31, 32, 33, 34, 31],
            &[("building:part", "yes"), ("height", "30")],
        ),
    ]
    .concat();
    let (world, report) = import(&document(&body));
    assert_eq!(
        report.counts.building_parts_merged,
        2,
        "{}",
        report.to_text()
    );
    assert_eq!(report.counts.building_parts_orphan, 1);
    assert_eq!(report.anomaly(Anomaly::BuildingPartWithoutOutline), 1);
    // One outline plus one orphan part: three of the four polygons are not obstacles of
    // their own.
    assert_eq!(report.counts.buildings, 2);
    assert_eq!(world.buildings.len(), 2);
    // FOLDED, not dropped. The outline is 50 m by its own tag and 120 m by the tallest
    // part standing inside it, and 120 m is the height of the structure. Dropping the
    // part and leaving the outline at 50 m — which is what the first repair did — threw
    // the real height away.
    let tower = world
        .buildings
        .iter()
        .find(|b| b.name.map(|n| world.symbols.resolve(n)) == Some("Tower"))
        .expect("the outline is a building");
    assert!(
        (tower.height_m - 120.0).abs() < 1e-9,
        "the outline kept {} m, not the 120 m part inside it",
        tower.height_m
    );
    assert_eq!(tower.height_source, HeightSource::FromParts);
    assert_eq!(report.counts.heights_from_parts, 1);
    // The orphan is the one that is kept, at its own height.
    assert!(
        world
            .buildings
            .iter()
            .any(|b| (b.height_m - 30.0).abs() < 1e-9)
    );
}

/// V1 (high regression). The height lives on the parts, so folding a part in has to fold
/// its HEIGHT in.
///
/// Midtown is mapped part-first: the Chrysler Building's `building=*` outline carries no
/// `height` at all and its 279 m tower is a `building:part`. An importer that drops the
/// part and reads the outline's own tags gives the Chrysler Building the 10 m land-use
/// default. On the Phase 1 extract that discarded 4 323 tagged heights, left 476
/// buildings materially too short and halved Midtown's built volume.
#[test]
fn an_untagged_outline_takes_the_height_of_its_tallest_part() {
    let square = |base: i64, lat: f64, lon: f64, size: f64| {
        [
            node(base, lat, lon),
            node(base + 1, lat, lon + size),
            node(base + 2, lat + size, lon + size),
            node(base + 3, lat + size, lon),
        ]
        .concat()
    };
    let body = [
        square(1, 0.0, 0.0, 0.002),         // the outline, about 222 m square
        square(11, 0.0002, 0.0002, 0.0004), // the tower
        square(21, 0.001, 0.001, 0.0004),   // a low wing
        // No height, no levels: everything this structure states is on its parts.
        way(
            100,
            &[1, 2, 3, 4, 1],
            &[("building", "yes"), ("name", "Chrysler")],
        ),
        way(
            101,
            &[11, 12, 13, 14, 11],
            &[("building:part", "yes"), ("height", "279")],
        ),
        way(
            102,
            &[21, 22, 23, 24, 21],
            &[("building:part", "yes"), ("height", "35")],
        ),
    ]
    .concat();
    let (world, report) = import(&document(&body));
    assert_eq!(world.buildings.len(), 1);
    let b = &world.buildings[0];
    assert!(
        (b.height_m - 279.0).abs() < 1e-9,
        "the outline is {} m tall; the tower inside it is 279 m",
        b.height_m
    );
    // The provenance says where that height came from: a part's tag, not this polygon's.
    assert_eq!(b.height_source, HeightSource::FromParts);
    assert_eq!(report.counts.heights_from_parts, 1);
    assert_eq!(report.counts.heights_defaulted, 0);
    assert_eq!(report.counts.building_parts_merged, 2);
}

/// V1 (high regression). A `building:part` multipolygon may borrow the `building=*`
/// outline way as one of its rings, and consuming that way deletes the structure.
///
/// This is how the Empire State Building is mapped: relation 10872054 is
/// `type=multipolygon` + `building:part=yes` for the five-storey base, and its `outer`
/// member is way 34633854 — the `building=office` way that carries the name and the
/// 443.2 m height. The importer treated every member way of a building multipolygon as
/// consumed, so the Empire State Building was absent from the Phase 1 world entirely and
/// the only thing left of it was its 55 m² spire.
#[test]
fn a_part_multipolygon_does_not_swallow_the_outline_it_borrows() {
    let square = |base: i64, lat: f64, lon: f64, size: f64| {
        [
            node(base, lat, lon),
            node(base + 1, lat, lon + size),
            node(base + 2, lat + size, lon + size),
            node(base + 3, lat + size, lon),
        ]
        .concat()
    };
    let body = [
        square(1, 0.0, 0.0, 0.001),
        square(11, 0.0002, 0.0002, 0.0002),
        way(
            100,
            &[1, 2, 3, 4, 1],
            &[
                ("building", "office"),
                ("height", "443.2"),
                ("name", "Empire State Building"),
            ],
        ),
        // The base part borrows the outline way as its outer ring and keeps a hole.
        way(101, &[11, 12, 13, 14, 11], &[]),
        relation(
            200,
            &[("way", 100, "outer"), ("way", 101, "inner")],
            &[
                ("type", "multipolygon"),
                ("building:part", "yes"),
                ("height", "17"),
            ],
        ),
    ]
    .concat();
    let (world, report) = import(&document(&body));
    let esb = world
        .buildings
        .iter()
        .find(|b| b.name.map(|n| world.symbols.resolve(n)) == Some("Empire State Building"))
        .expect("the Empire State Building is in the world");
    assert!((esb.height_m - 443.2).abs() < 1e-9, "{}", esb.height_m);
    assert_eq!(report.anomaly(Anomaly::OutlineInPartRelation), 1);
}

/// V10. The spire guard failed on a strict inequality at exactly the boundary.
///
/// OSM way 137425145 is the Empire State Building's mast: `building:part=yes`,
/// `height=443.2`, `min_height=330`, `roof:height=113.2`, `roof:shape=pyramidal`. Its
/// structural top is 443.2 − 113.2 = 330.0, which is EQUAL to its `min_height`, so
/// `structural > min_height_m` was false and the 443.2 m thin prism stood: a 55 m²
/// antenna blocking rays across Midtown.
#[test]
fn a_spire_whose_structural_top_equals_its_base_is_still_capped() {
    let square = |base: i64, lat: f64, lon: f64, size: f64| {
        [
            node(base, lat, lon),
            node(base + 1, lat, lon + size),
            node(base + 2, lat + size, lon + size),
            node(base + 3, lat + size, lon),
        ]
        .concat()
    };
    // An orphan part, so that the mast is emitted and its own height can be read.
    let body = [
        square(1, 0.0, 0.0, 0.0001),
        way(
            100,
            &[1, 2, 3, 4, 1],
            &[
                ("building:part", "yes"),
                ("height", "443.2"),
                ("min_height", "330"),
                ("roof:height", "113.2"),
                ("roof:shape", "pyramidal"),
            ],
        ),
    ]
    .concat();
    let (world, report) = import(&document(&body));
    assert_eq!(world.buildings.len(), 1);
    let mast = &world.buildings[0];
    assert!(
        (mast.height_m - 330.0).abs() < 1e-9,
        "the mast is {} m tall, not its 330 m structural top",
        mast.height_m
    );
    assert!((mast.min_height_m - 330.0).abs() < 1e-9);
    assert_eq!(report.anomaly(Anomaly::SpireHeightCapped), 1);
    assert_eq!(report.counts.buildings_spire_capped, 1);
}

/// V4/W1. There is no default speed preset, because a default speed limit is a
/// statement about a jurisdiction.
///
/// The first repair made the presets named and selectable but left `sumo-german` as the
/// silent default, so every caller who did not pass one still got 404 driving lanes
/// above 90 km/h on Midtown side streets.
#[test]
fn an_import_without_a_speed_preset_is_refused() {
    let options = OsmOptions::default().imported_at("2026-09-18T00:00:00Z");
    assert_eq!(options.highway_preset, None);
    let refused = options.validate().expect_err("no preset, no import");
    let message = refused.to_string();
    assert!(message.contains("highway_preset"), "{message}");
    assert!(message.contains("sumo-german"), "{message}");
    assert!(message.contains("urban-us-nyc"), "{message}");

    // And the refusal is the import's, not just the option check's.
    let xml = document(&[
        crossroads_nodes(),
        way(10, &[1, 2, 3], &[("highway", "secondary")]),
    ]
    .concat());
    assert!(import_osm_bytes(xml.as_bytes(), "fixture", &options).is_err());

    // Naming one imports.
    let named = options.highway_preset(HighwayPreset::UrbanUsNyc);
    named.validate().expect("a named preset validates");
    let (world, report) = import_osm_bytes(xml.as_bytes(), "fixture", &named).expect("imports");
    assert_eq!(
        report.highway_preset.map(HighwayPreset::label),
        Some("urban-us-nyc")
    );
    assert!(
        world
            .roads
            .lanes()
            .iter()
            .filter(|l| l.kind == LaneKind::Driving)
            .all(|l| (l.speed_limit_mps - 11.176).abs() < 1e-9)
    );
}

/// V4/W1. A preset from the wrong jurisdiction is invisible in the counts — every lane
/// it touched is a lane with no tag to contradict it — so the importer checks the
/// defaults it used against the limits the source itself states, and says so loudly.
#[test]
fn the_report_calls_out_class_defaults_above_the_tagged_distribution() {
    // Twelve tagged 25 mph residential streets (one lane each way, 24 drivable lanes) and
    // two untagged secondary avenues (two lanes each way, 8 drivable lanes).
    let mut body = String::new();
    for i in 0..12i64 {
        let lat = -0.0018 + 0.0003 * i as f64;
        body.push_str(&node(100 + 2 * i, lat, -0.0015));
        body.push_str(&node(101 + 2 * i, lat, 0.0015));
        body.push_str(&way(
            200 + i,
            &[100 + 2 * i, 101 + 2 * i],
            &[
                ("highway", "residential"),
                ("maxspeed", "25 mph"),
                ("name", "Tagged Street"),
            ],
        ));
    }
    for i in 0..2i64 {
        let lon = -0.0005 + 0.001 * i as f64;
        body.push_str(&node(300 + 2 * i, -0.0018, lon));
        body.push_str(&node(301 + 2 * i, 0.0018, lon));
        body.push_str(&way(
            400 + i,
            &[300 + 2 * i, 301 + 2 * i],
            &[("highway", "secondary"), ("name", "Untagged Avenue")],
        ));
    }
    let xml = document(&body);

    let (_, german) = import_with(&xml, &opts());
    let text = german.to_text();
    assert!(german.speed_audit.fired, "{text}");
    assert_eq!(german.anomaly(Anomaly::ClassDefaultsAboveTaggedSpeeds), 1);
    assert!(text.contains("speed audit       WARNING"), "{text}");
    assert_eq!(german.speed_audit.far_above_lanes, 8, "{text}");
    assert!((german.speed_audit.tagged_p95_mps - 11.176).abs() < 1e-9);

    // The urban preset's defaults sit inside the distribution the source states, so the
    // same world imports quietly.
    let (_, nyc) = import_with(&xml, &opts().highway_preset(HighwayPreset::UrbanUsNyc));
    assert!(!nyc.speed_audit.fired, "{}", nyc.to_text());
    assert_eq!(nyc.anomaly(Anomaly::ClassDefaultsAboveTaggedSpeeds), 0);
    assert_eq!(nyc.speed_audit.far_above_lanes, 0);
    assert!(nyc.to_text().contains("speed audit       ok"));
}

/// V2/W4 (high). An underground station is not a 10 m obstacle on the roadway.
///
/// Three of them — Times Square, Herald Square and Park Avenue at Grand Central — put
/// 205 361 m² of phantom 10 m prisms over exactly the intersections a V2X study models
/// NLOS propagation at. Their tags said so unambiguously.
#[test]
fn an_underground_station_is_not_an_obstacle() {
    let station = |tags: &[(&str, &str)]| {
        [
            node(1, 0.0, 0.0),
            node(2, 0.0, 0.001),
            node(3, 0.001, 0.001),
            node(4, 0.001, 0.0),
            node(10, 0.0005, -0.001),
            node(11, 0.0005, 0.002),
            way(100, &[1, 2, 3, 4, 1], tags),
            way(
                200,
                &[10, 11],
                &[("highway", "primary"), ("name", "Broadway")],
            ),
        ]
        .concat()
    };
    // OSM way 812938420's own tags.
    let (world, report) = import(&document(&station(&[
        ("building", "train_station"),
        ("location", "underground"),
        ("layer", "-1"),
        ("underground", "yes"),
        ("railway", "station"),
        ("name", "Times Square-42nd Street"),
    ])));
    assert_eq!(report.counts.buildings, 0, "{}", report.to_text());
    assert_eq!(report.counts.buildings_subsurface, 1);
    assert_eq!(report.anomaly(Anomaly::SubsurfaceStructure), 1);
    assert!(world.buildings.is_empty());
    // No drivable lane vertex is inside a building any more, because there is no building.
    for lane in world.roads.lanes() {
        for point in &lane.centreline {
            assert!(
                !world.buildings.iter().any(|b| b.contains_2d(*point)),
                "a lane still runs through a building"
            );
        }
    }

    // The control: the same polygon above ground is a building, as it should be. A station
    // with a real head house carries `building=*` and no underground tag.
    let (world, report) = import(&document(&station(&[
        ("building", "train_station"),
        ("railway", "station"),
        ("name", "Grand Central Terminal"),
    ])));
    assert_eq!(report.counts.buildings, 1);
    assert_eq!(report.counts.buildings_subsurface, 0);
    assert_eq!(world.buildings.len(), 1);
}

/// V4/W1 (high). The class default speed is a named, swappable preset with a cited source
/// per row, the report says which one ran, and a tagged way is unaffected either way.
///
/// 404 drivable lanes (16.7 %) of the Phase 1 world carried a limit 2.5 to 3.5× the real
/// one — 393 Midtown side-street lanes at 100 km/h — because SUMO's German design speeds
/// were a hard-coded constant with no source on the card.
#[test]
fn the_speed_default_is_a_named_preset_and_a_tag_still_wins() {
    let body = [
        node(1, 0.0, -0.001),
        node(2, 0.0, 0.0),
        node(3, 0.0, 0.001),
        node(4, 0.001, 0.0),
        way(
            10,
            &[1, 2],
            &[
                ("highway", "secondary"),
                ("oneway", "yes"),
                ("name", "West 49th Street"),
            ],
        ),
        way(
            11,
            &[2, 3],
            &[
                ("highway", "secondary"),
                ("oneway", "yes"),
                ("maxspeed", "25 mph"),
                ("name", "Seventh Avenue"),
            ],
        ),
        way(12, &[2, 4], &[("highway", "secondary"), ("oneway", "yes")]),
    ]
    .concat();
    let xml = document(&body);

    let limit_of = |world: &World, name: &str| -> f64 {
        let symbol = world
            .symbols
            .get(name)
            .unwrap_or_else(|| panic!("{name} is interned"));
        world
            .roads
            .edges()
            .iter()
            .filter(|e| e.name == Some(symbol))
            .flat_map(|e| e.lanes.iter())
            .map(|l| world.roads.lane(*l).speed_limit_mps)
            .fold(0.0f64, f64::max)
    };

    let (german, report) = import_with(&xml, &opts());
    assert_eq!(report.highway_preset.map(HighwayPreset::label), Some("sumo-german"));
    assert!(report.to_text().contains("preset sumo-german"));
    // Two lanes on the tagged avenue; two each on the two untagged streets.
    assert_eq!(report.counts.speeds_tagged, 2, "{}", report.to_text());
    assert_eq!(report.counts.speeds_defaulted, 4);
    assert!(
        (limit_of(&german, "West 49th Street") - 27.78).abs() < 1e-9,
        "SUMO's design speed is 100 km/h"
    );
    assert!((limit_of(&german, "Seventh Avenue") - 11.176).abs() < 1e-9);

    let urban = opts().highway_preset(HighwayPreset::UrbanUsNyc);
    let (nyc, report) = import_with(&xml, &urban);
    assert_eq!(
        report.highway_preset.map(HighwayPreset::label),
        Some("urban-us-nyc")
    );
    assert!(report.to_text().contains("preset urban-us-nyc"));
    assert!(
        (limit_of(&nyc, "West 49th Street") - 11.176).abs() < 1e-9,
        "the NYC citywide default is 25 mph, not 100 km/h"
    );
    // The tag wins under either preset.
    assert!((limit_of(&nyc, "Seventh Avenue") - 11.176).abs() < 1e-9);
    // Which preset ran is on the record with its citation.
    let recorded = nyc
        .provenance
        .transformations
        .iter()
        .find(|t| t.name == "class-defaults")
        .expect("the preset is recorded")
        .to_wire_string();
    assert!(recorded.contains("preset=urban-us-nyc"), "{recorded}");
    assert!(recorded.contains("25 mph"), "{recorded}");
}

/// R1 (high). Offsetting a bend tighter than the lane offset used to invert that part of
/// the lane, so the centreline crossed itself and arc length stopped being injective in
/// space. 56 lanes of the Phase 1 world were affected, 8 of them driving lanes.
#[test]
fn no_lane_centreline_crosses_itself() {
    // A zigzag with about 2.5 m between its vertices, on a carriageway of eight 3.658 m
    // lanes — so the lane offsets reach 12.8 m, five times the bend radius. The long
    // approach and exit are there so that junction trimming does not cut the bends away
    // before they can be offset.
    let body = [
        node(1, 0.0, -0.0005),
        node(2, 0.0, 0.0),
        node(3, 0.00002, 0.00001),
        node(4, 0.00004, 0.0),
        node(5, 0.00006, 0.00001),
        node(6, 0.00008, 0.0),
        node(7, 0.0008, 0.0),
        way(
            10,
            &[1, 2, 3, 4, 5, 6, 7],
            &[
                ("highway", "motorway"),
                ("lanes", "8"),
                ("oneway", "yes"),
                ("name", "Switchback"),
            ],
        ),
    ]
    .concat();
    let (world, report) = import(&document(&body));
    // The property first: no lane in the world crosses itself.
    for lane in world.roads.lanes() {
        assert!(
            !self_intersects(&lane.centreline),
            "lane {} crosses itself: {:?}",
            lane.id,
            lane.centreline
        );
    }
    // Then that the repair really was exercised, and was reported rather than silent.
    assert!(
        report.counts.lanes_repaired > 0,
        "the fixture does not exercise the repair: {}",
        report.to_text()
    );
    assert_eq!(
        report.anomaly(Anomaly::SelfIntersectingLane),
        report.counts.lanes_repaired,
        "every repair is counted as an anomaly"
    );
    // And the ordinary world needs no repair at all.
    let (world, report) = import(&mixed_fixture());
    assert_eq!(report.counts.lanes_repaired, 0);
    assert_eq!(report.anomaly(Anomaly::SelfIntersectingLane), 0);
    for lane in world.roads.lanes() {
        assert!(!self_intersects(&lane.centreline));
    }
}

/// V5 (medium). A way that leaves the requested box is cut at the box rather than
/// imported whole, so a lane no longer crosses four intersections that are not in the
/// extract. The extent is then the request plus the margin, not whatever the source held.
#[test]
fn an_overhanging_way_is_clipped_at_the_box() {
    let body = [
        node(1, 0.0, -0.01),
        node(2, 0.0, 0.0),
        node(3, 0.0, 0.01),
        way(
            10,
            &[1, 2, 3],
            &[("highway", "residential"), ("name", "West 48th Street")],
        ),
    ]
    .concat();
    let xml = document(&body);
    // About 222 m of latitude and 222 m of longitude at the equator.
    let bbox = v2xw_world::GeoBbox::new(-0.001, -0.001, 0.001, 0.001);

    let clipped = OsmOptions {
        bbox: Some(bbox),
        ..opts()
    };
    let (world, report) = import_with(&xml, &clipped);
    assert_eq!(report.bbox_clip.label(), "clip");
    assert!(report.counts.ways_clipped >= 1, "{}", report.to_text());
    assert!(report.counts.clipped_runs >= 1);
    assert!(report.anomaly(Anomaly::ClippedGeometry) >= 1);
    let (east, _) = report.extent_m.expect("the extent is measured");
    // The request is 222 m wide; the clip keeps one 50 m margin on each side.
    assert!(
        east < 222.0 + 2.0 * report.bbox_clip.margin_m() + 20.0,
        "the world is {east} m across for a 222 m request"
    );
    // The frame is still the request's corner, and the box is honoured to the margin.
    assert!(world.bbox.min.x >= -report.bbox_clip.margin_m() - 1.0);

    // The other arm keeps the way whole, and says so, so the old behaviour is available
    // and recorded rather than implied.
    let whole = OsmOptions {
        bbox: Some(bbox),
        bbox_clip: BboxClip::KeepWhole,
        ..opts()
    };
    let (_, report_whole) = import_with(&xml, &whole);
    assert_eq!(report_whole.bbox_clip.label(), "keep-whole");
    assert_eq!(report_whole.counts.ways_clipped, 0);
    let (east_whole, _) = report_whole.extent_m.expect("the extent is measured");
    assert!(
        east_whole > 2000.0,
        "keeping the way whole spans the whole 2.2 km way, not {east_whole} m"
    );
    assert!(
        east_whole > east * 5.0,
        "clipping did not bound the extent: {east} against {east_whole}"
    );
}

/// V6/W2 (medium). Lane width comes from the way's own `width`, then from its class —
/// not from one global 3.50 m for every carriageway in the city.
#[test]
fn lane_width_comes_from_the_tag_then_the_class() {
    let body = [
        node(1, 0.0, -0.001),
        node(2, 0.0, 0.001),
        node(3, 0.0005, -0.001),
        node(4, 0.0005, 0.001),
        node(5, 0.001, -0.001),
        node(6, 0.001, 0.001),
        way(
            10,
            &[1, 2],
            &[
                ("highway", "primary"),
                ("lanes", "4"),
                ("width", "20"),
                ("name", "Tagged Avenue"),
            ],
        ),
        way(
            11,
            &[3, 4],
            &[("highway", "primary"), ("lanes", "4"), ("name", "Avenue")],
        ),
        way(12, &[5, 6], &[("highway", "service"), ("name", "Alley")]),
    ]
    .concat();
    let (world, report) = import(&document(&body));
    let width_of = |name: &str| -> f64 {
        let symbol = world.symbols.get(name).expect("interned");
        world
            .roads
            .edges()
            .iter()
            .filter(|e| e.name == Some(symbol))
            .flat_map(|e| e.lanes.iter())
            .map(|l| world.roads.lane(*l).width_m)
            .fold(0.0f64, f64::max)
    };
    assert!(
        (width_of("Tagged Avenue") - 5.0).abs() < 1e-9,
        "20 m over 4 lanes"
    );
    assert!(
        (width_of("Avenue") - 3.353).abs() < 1e-9,
        "11 ft, NACTO arterial"
    );
    assert!(
        (width_of("Alley") - 2.743).abs() < 1e-9,
        "9 ft, AASHTO local"
    );
    assert!(
        width_of("Avenue") > width_of("Alley"),
        "an avenue and an alley no longer get identical geometry"
    );
    assert!(report.counts.widths_tagged > 0, "{}", report.to_text());
    assert!(report.counts.widths_defaulted > 0);
    // The global option is still there as the last resort, and a tag still beats it.
    let forced = OsmOptions {
        lane_width_m: Some(4.0),
        ..opts()
    };
    let (world, _) = import_with(&document(&body), &forced);
    let symbol = world.symbols.get("Avenue").expect("interned");
    let forced_width = world
        .roads
        .edges()
        .iter()
        .filter(|e| e.name == Some(symbol))
        .flat_map(|e| e.lanes.iter())
        .map(|l| world.roads.lane(*l).width_m)
        .fold(0.0f64, f64::max);
    assert!((forced_width - 4.0).abs() < 1e-9);
}

/// R2 (medium). A `maxspeed` is a vehicle speed limit, so it does not become a footway's
/// walking pace or a 60 km/h staircase. The ignored tag is counted, not silently dropped.
#[test]
fn a_maxspeed_on_a_footway_is_ignored_and_counted() {
    let body = [
        node(1, 0.0, -0.001),
        node(2, 0.0, 0.001),
        node(3, 0.0005, -0.001),
        node(4, 0.0005, 0.001),
        node(5, 0.001, -0.001),
        node(6, 0.001, 0.001),
        way(
            10,
            &[1, 2],
            &[("highway", "footway"), ("maxspeed", "25 mph")],
        ),
        way(11, &[3, 4], &[("highway", "steps"), ("maxspeed", "60")]),
        way(12, &[5, 6], &[("highway", "cycleway"), ("maxspeed", "60")]),
    ]
    .concat();
    let (world, report) = import(&document(&body));
    assert_eq!(
        report.anomaly(Anomaly::SpeedTagIgnored),
        3,
        "{}",
        report.to_text()
    );
    for lane in world.roads.lanes() {
        match lane.kind {
            LaneKind::Sidewalk => assert!(
                lane.speed_limit_mps <= 1.39,
                "a pavement at {} m/s",
                lane.speed_limit_mps
            ),
            LaneKind::Cycle => assert!(
                lane.speed_limit_mps <= 5.56,
                "a cycleway at {} m/s",
                lane.speed_limit_mps
            ),
            _ => {}
        }
    }
}

/// R3 (medium) and V8 (low). A junction whose arms are too short to trim leaves the
/// approach and departure lanes touching. Such a movement used to be emitted as a direct
/// permitted connection that neither the restriction machinery nor the conflict matrix
/// ever saw, so a banned turn stayed permitted. It is now an ordinary movement.
///
/// The same fixture exercises V8: the lanes it leaves are about a metre long, which is
/// what `short-driving-lane` names.
#[test]
fn a_restriction_applies_even_where_the_lanes_touch() {
    // 1e-5 degrees is about 1.11 m, so every arm is shorter than its own trim radius.
    let body = [
        node(1, 0.0, -0.00001),
        node(2, 0.0, 0.0),
        node(3, 0.0, 0.00001),
        node(4, 0.00001, 0.0),
        way(
            10,
            &[1, 2],
            &[("highway", "primary"), ("oneway", "yes"), ("lanes", "1")],
        ),
        way(
            11,
            &[2, 3],
            &[("highway", "primary"), ("oneway", "yes"), ("lanes", "1")],
        ),
        way(
            12,
            &[2, 4],
            &[("highway", "primary"), ("oneway", "yes"), ("lanes", "1")],
        ),
        relation(
            20,
            &[("way", 10, "from"), ("node", 2, "via"), ("way", 11, "to")],
            &[("type", "restriction"), ("restriction", "no_straight_on")],
        ),
    ]
    .concat();
    let (world, report) = import(&document(&body));
    assert_eq!(
        report.counts.restrictions_applied,
        1,
        "the restriction never reached the movement: {}",
        report.to_text()
    );
    let banned: Vec<_> = world
        .roads
        .connections()
        .iter()
        .filter(|c| !c.permitted)
        .collect();
    assert!(!banned.is_empty(), "a banned turn is still permitted");
    for connection in &banned {
        assert_eq!(connection.direction, TurnDirection::Straight);
    }
    // V8: the slivers this leaves are named, with a count and a threshold, rather than
    // being left for a car-following model to discover.
    assert!(
        report.counts.short_driving_lanes > 0,
        "{}",
        report.to_text()
    );
    assert_eq!(
        report.anomaly(Anomaly::ShortDrivingLane),
        report.counts.short_driving_lanes
    );
    assert_eq!(report.min_useful_lane_m, 5.0);
    assert!(report.to_text().contains("shorter than 5 m"));
}

/// R4 (medium). A restriction is matched by the turn it names and by the way that reaches
/// the via node, not by way id alone — so a `no_left_turn` on a street that passes
/// through the junction twice does not also ban the legal right turn from the opposite
/// approach.
#[test]
fn a_restriction_does_not_ban_the_opposite_approach() {
    let body = [
        node(1, 0.0, -0.001),
        node(2, 0.0, 0.0),
        node(3, 0.0, 0.001),
        node(5, 0.001, 0.0),
        // One two-way street through the junction, so both approaches carry way 10.
        way(
            10,
            &[1, 2, 3],
            &[("highway", "primary"), ("lanes", "2"), ("name", "Main")],
        ),
        way(
            12,
            &[2, 5],
            &[("highway", "primary"), ("lanes", "2"), ("name", "North")],
        ),
        relation(
            20,
            &[("way", 10, "from"), ("node", 2, "via"), ("way", 12, "to")],
            &[("type", "restriction"), ("restriction", "no_left_turn")],
        ),
    ]
    .concat();
    let (world, report) = import(&document(&body));
    assert_eq!(report.counts.restrictions_applied, 1);
    let banned: Vec<_> = world
        .roads
        .connections()
        .iter()
        .filter(|c| !c.permitted)
        .collect();
    assert!(!banned.is_empty(), "the left turn is banned");
    for connection in &banned {
        assert_eq!(
            connection.direction,
            TurnDirection::Left,
            "a legal turn from the other approach was banned too"
        );
    }
    // The right turn onto the same street from the opposite approach survives.
    assert!(
        world
            .roads
            .connections()
            .iter()
            .any(|c| c.permitted && c.direction == TurnDirection::Right),
        "the legal right turn from the opposite approach was removed"
    );
}

/// R8 (medium). A document that is not an OSM extract is an error, not a valid, empty,
/// content-addressed world that then goes into the cache under its own hash.
#[test]
fn a_document_that_is_not_an_extract_is_refused() {
    // Overpass reports a timeout as a well-formed <osm> document with a <remark>.
    let overpass = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                    <osm version=\"0.6\" generator=\"Overpass API\">\n\
                    <remark>runtime error: Query timed out in \"query\" at line 3</remark>\n\
                    </osm>\n";
    let error = import_osm_bytes(overpass.as_bytes(), "overpass", &opts())
        .expect_err("an Overpass failure is not a world");
    let text = error.to_string();
    assert!(text.contains("Query timed out"), "{text}");

    // An HTML error page has no <osm> root at all.
    let html = "<html><head><title>502 Bad Gateway</title></head>\
                <body><h1>502 Bad Gateway</h1></body></html>";
    let error = import_osm_bytes(html.as_bytes(), "gateway", &opts())
        .expect_err("an HTML page is not a world");
    assert!(error.to_string().contains("<osm> root"), "{error}");

    // And a real but empty <osm> document is refused too: an empty bounding box and a
    // failed download must not produce the same world.
    let error = import_osm_bytes(document("").as_bytes(), "empty", &opts())
        .expect_err("an empty document is not a world");
    assert!(error.to_string().contains("no nodes"), "{error}");
}

/// V9 (low). Road junctions can be iterated on their own, and the report quotes
/// signalisation against the junctions that have real arms as well as against every
/// junction in the world.
#[test]
fn road_junctions_can_be_told_from_footway_intersections() {
    let (world, report) = import(&mixed_fixture());
    let roads = road_junction_ids(&world);
    assert_eq!(roads.len() as u64, report.counts.road_junctions);
    assert!(
        report.counts.road_junctions < report.counts.junctions,
        "the fixture has footway junctions too: {}",
        report.to_text()
    );
    assert!(report.counts.road_junctions > 0);
    // Every junction the helper names really does have a drivable arm, and every one it
    // leaves out really does not.
    for junction in world.roads.junctions() {
        let drivable = world
            .roads
            .edges()
            .iter()
            .filter(|e| e.road_class != v2xw_world::RoadClass::Internal)
            .filter(|e| e.from == junction.id || e.to == junction.id)
            .any(|e| {
                e.lanes
                    .iter()
                    .any(|l| world.roads.lane(*l).admits(ClassMask::MOTOR_TRAFFIC))
            });
        assert_eq!(roads.contains(&junction.id), drivable, "{}", junction.id);
    }
    // Both figures are in the report, so the headline share cannot be read against the
    // wrong denominator.
    assert!(report.to_text().contains("road junctions"));
    assert!(report.counts.major_road_junctions <= report.counts.road_junctions);
    assert!(report.counts.signalised_major_junctions <= report.counts.signalised_junctions);
}

/// V10 (low). A height that runs to a spire tip is cut back to the structural top, so an
/// antenna mast is not a solid obstacle; a pitched roof, which is building mass, is not.
#[test]
fn a_spire_height_is_cut_back_to_the_structural_top() {
    let square = |base: i64, lat: f64, lon: f64| {
        [
            node(base, lat, lon),
            node(base + 1, lat, lon + 0.0004),
            node(base + 2, lat + 0.0004, lon + 0.0004),
            node(base + 3, lat + 0.0004, lon),
        ]
        .concat()
    };
    let body = [
        square(1, 0.0, 0.0),
        square(11, 0.0, 0.001),
        way(
            100,
            &[1, 2, 3, 4, 1],
            &[
                ("building", "yes"),
                ("height", "443.2"),
                ("roof:height", "113.2"),
                ("roof:shape", "spire"),
                ("name", "Spire"),
            ],
        ),
        way(
            101,
            &[11, 12, 13, 14, 11],
            &[
                ("building", "yes"),
                ("height", "12"),
                ("roof:height", "3"),
                ("roof:shape", "gabled"),
                ("name", "Pitched Roof"),
            ],
        ),
    ]
    .concat();
    let (world, report) = import(&document(&body));
    assert_eq!(report.counts.buildings, 2);
    assert_eq!(
        report.counts.buildings_spire_capped,
        1,
        "{}",
        report.to_text()
    );
    assert_eq!(report.anomaly(Anomaly::SpireHeightCapped), 1);
    let height_of = |name: &str| -> f64 {
        let symbol = world.symbols.get(name).expect("interned");
        world
            .buildings
            .iter()
            .find(|b| b.name == Some(symbol))
            .expect("the building")
            .height_m
    };
    assert!(
        (height_of("Spire") - 330.0).abs() < 1e-9,
        "443.2 m to the tip, 330 m of structure"
    );
    assert!(
        (height_of("Pitched Roof") - 12.0).abs() < 1e-9,
        "a pitched roof is building mass and stays"
    );
}

/// V7/W6 (low). The simplifications this importer does not implement are reported as
/// skipped and recorded as a `not-applied` transformation, and asking for one changes
/// nothing about the world — which is the point: a simplification that silently did not
/// run would be indistinguishable from one that ran and found nothing.
#[test]
fn asking_for_an_unimplemented_simplification_records_that_it_did_not_run() {
    let asked = OsmOptions {
        simplify: OsmSimplifications {
            collapse_trivial_junctions: true,
            merge_dual_carriageways: true,
            snap_parallel_footways: true,
        },
        ..opts()
    };
    let (world, report) = import_with(&mixed_fixture(), &asked);
    for name in [
        "merge-dual-carriageways",
        "snap-parallel-footways",
        "merge-dog-leg-junctions",
    ] {
        assert!(
            report.skipped_simplifications.contains(&name.to_string()),
            "{name} is not reported as skipped"
        );
        assert!(
            world
                .provenance
                .transformations
                .iter()
                .any(|t| { t.name == "not-applied" && t.to_wire_string().contains(name) }),
            "{name} has no not-applied transformation"
        );
    }
    // Nothing silently happened: the world is the one built without asking.
    let (baseline, baseline_report) = import(&mixed_fixture());
    assert_eq!(world.content_hash, baseline.content_hash);
    assert_eq!(
        report.counts.junctions, baseline_report.counts.junctions,
        "a junction was merged after all"
    );
}

// ---------------------------------------------------------------------------
// The real city (D7)
// ---------------------------------------------------------------------------

/// Where the Manhattan extract lives. It is gitignored: 30 MB of ODbL data does not belong
/// in the repository, and the importer is city-agnostic, so the file is a fixture rather
/// than a dependency.
const MANHATTAN: &str = "../../worlds/cache/manhattan.osm.xml";

/// The largest connected component of the drivable graph, in lanes.
///
/// Connectivity is taken as undirected on purpose: a one-way grid is strongly connected
/// only through its whole cycle structure, and what this test is checking is that the
/// importer did not shatter the network into islands, not that every pair of lanes is
/// mutually reachable.
fn largest_drivable_component(world: &World) -> usize {
    let drivable: BTreeSet<LaneId> = world
        .roads
        .lanes()
        .iter()
        .filter(|l| l.admits(ClassMask::MOTOR_TRAFFIC))
        .map(|l| l.id)
        .collect();
    let mut neighbours: Vec<Vec<LaneId>> = vec![Vec::new(); world.roads.lanes().len()];
    for connection in world.roads.connections() {
        if !drivable.contains(&connection.from_lane) || !drivable.contains(&connection.to_lane) {
            continue;
        }
        neighbours[connection.from_lane.as_usize()].push(connection.to_lane);
        neighbours[connection.to_lane.as_usize()].push(connection.from_lane);
    }
    let mut seen = vec![false; world.roads.lanes().len()];
    let mut best = 0;
    for start in &drivable {
        if seen[start.as_usize()] {
            continue;
        }
        let mut size = 0;
        let mut queue = VecDeque::from([*start]);
        seen[start.as_usize()] = true;
        while let Some(lane) = queue.pop_front() {
            size += 1;
            for next in &neighbours[lane.as_usize()] {
                if !seen[next.as_usize()] {
                    seen[next.as_usize()] = true;
                    queue.push_back(*next);
                }
            }
        }
        best = best.max(size);
    }
    best
}

/// The real Manhattan extract of D7.
///
/// Ignored by default: the file is 30 MB, gitignored, and the import takes a few seconds.
/// Run it with `cargo test -p v2xw-world --test osm -- --ignored --nocapture`, which also
/// prints the import report.
#[test]
#[ignore = "needs worlds/cache/manhattan.osm.xml, which is gitignored and 30 MB"]
fn manhattan_imports_into_a_connected_city() {
    let start = std::time::Instant::now();
    let (world, report) = import_osm(MANHATTAN, &opts()).expect("Manhattan imports");
    let elapsed = start.elapsed();
    println!("{}", report.to_text());
    println!("import took {} ms", elapsed.as_millis());
    println!(
        "content hash {}",
        v2xw_world::hash::content_hash_hex(&world)
    );

    // The measured contents of the extract (D7): 58,434 nodes, 13,553 ways, 1,129
    // relations, 7,371 buildings, 438 traffic-signal nodes, about 940 drivable ways.
    assert!((50_000..70_000).contains(&report.counts.osm_nodes));
    assert!((12_000..15_000).contains(&report.counts.osm_ways));
    assert!(
        (700..1_400).contains(&report.counts.drivable_ways),
        "drivable ways: {}",
        report.counts.drivable_ways
    );
    assert!(
        (2_500..4_000).contains(&report.counts.buildings),
        "buildings: {}",
        report.counts.buildings
    );
    assert_eq!(report.counts.traffic_signal_nodes, 438);
    assert!(
        report.counts.signalised_junctions > 200,
        "signalised junctions: {}",
        report.counts.signalised_junctions
    );
    // Height coverage: the extract states an explicit height for 94 % of its buildings —
    // on the outline for most of them and, in Midtown, on a `building:part` inside it
    // (V1), which is a surveyed height just the same. Counting only `heights_tagged`
    // would call the Chrysler Building's 279 m unsourced.
    let surveyed = report.counts.heights_tagged + report.counts.heights_from_parts;
    let tagged_share = surveyed as f64 / report.counts.buildings.max(1) as f64;
    assert!(tagged_share > 0.85, "tagged height share: {tagged_share}");
    assert!(
        report.counts.heights_from_parts > 300,
        "heights from parts: {}",
        report.counts.heights_from_parts
    );

    // The collapse must actually collapse: OSM splits Manhattan's avenues into many ways.
    assert!(
        report.counts.junctions_collapsed > 100,
        "junctions collapsed: {}",
        report.counts.junctions_collapsed
    );

    // Sanity on the world itself.
    world.validate().expect("the imported world is valid");
    let counts = world.counts();
    assert!(counts.junctions > 500 && counts.junctions < 20_000);
    assert!(counts.lanes > 5_000);
    assert!(world.roads.total_lane_length_m() > 100_000.0);

    // The drivable graph is one city, not an archipelago.
    let drivable = world
        .roads
        .lanes()
        .iter()
        .filter(|l| l.admits(ClassMask::MOTOR_TRAFFIC))
        .count();
    let largest = largest_drivable_component(&world);
    let share = largest as f64 / drivable as f64;
    println!("largest drivable component: {largest} of {drivable} lanes ({share:.3})");
    assert!(
        share > 0.90,
        "the largest drivable component holds only {share:.3} of the drivable lanes"
    );

    // Every float on its grid, and both writers accept it.
    let mut off = 0;
    world.scan_exported_floats(&mut |_, value, quantum| {
        if !is_on_grid(value, quantum) {
            off += 1;
        }
    });
    assert_eq!(off, 0);
    let payload = serde_vwp::write(&world).expect("the vwp payload writes");
    assert_eq!(
        serde_vwp::verify(&payload.bytes).expect("the payload verifies"),
        payload.content_hash
    );
    println!(
        "vwp payload {} bytes, {} precision warnings",
        payload.bytes.len(),
        payload.precision_warnings.len()
    );

    // R1. The acceptance criterion the code reviewer asked for: not one lane in the whole
    // city has a centreline that crosses itself. 56 did, 8 of them driving lanes, and
    // arc length stopped being injective in space on every one.
    let crossing: Vec<_> = world
        .roads
        .lanes()
        .iter()
        .filter(|l| self_intersects(&l.centreline))
        .map(|l| l.id)
        .collect();
    assert!(crossing.is_empty(), "self-intersecting lanes: {crossing:?}");
    println!(
        "lanes repaired after offsetting: {}",
        report.counts.lanes_repaired
    );

    // V1. Every structure once: the part fragments are folded into their outlines, so the
    // count is the real one rather than 2.44x it.
    println!(
        "buildings {} ({} parts folded, {} orphan parts, {} subsurface dropped, {} spires \
         capped)",
        report.counts.buildings,
        report.counts.building_parts_merged,
        report.counts.building_parts_orphan,
        report.counts.buildings_subsurface,
        report.counts.buildings_spire_capped
    );
    assert!(
        report.counts.building_parts_merged > 1_000,
        "the extract is full of building:part fragments and they were not folded"
    );

    // V2/W4. No underground station is an obstacle any more, so the roadway through Times
    // Square, Herald Square and Grand Central is clear.
    assert!(
        report.counts.buildings_subsurface > 0,
        "the extract carries 214 location=underground ways"
    );
    let inside = world
        .roads
        .lanes()
        .iter()
        .filter(|l| l.admits(ClassMask::MOTOR_TRAFFIC))
        .flat_map(|l| l.centreline.iter())
        .filter(|p| world.buildings.iter().any(|b| b.contains_2d(**p)))
        .count();
    let vertices = world
        .roads
        .lanes()
        .iter()
        .filter(|l| l.admits(ClassMask::MOTOR_TRAFFIC))
        .map(|l| l.centreline.len())
        .sum::<usize>();
    let share = inside as f64 / vertices.max(1) as f64;
    println!("drivable lane vertices inside a building: {inside} of {vertices} ({share:.4})");
    assert!(
        share < 0.05,
        "10.6 % of drivable lane vertices were inside a building; now {share:.4}"
    );

    // V4/W1, V6/W2, V8, V9: the figures the report must now state.
    println!(
        "speeds: {} tagged, {} from the {} preset",
        report.counts.speeds_tagged,
        report.counts.speeds_defaulted,
        report.highway_preset.map_or("(none)", HighwayPreset::label)
    );
    // V4/W1's headline: how many driving lanes carry a limit above 90 km/h, which in a
    // city is always the class default rather than a tag.
    let over_90 = |world: &World| {
        world
            .roads
            .lanes()
            .iter()
            .filter(|l| l.kind == LaneKind::Driving && l.speed_limit_mps > 25.0)
            .count()
    };
    let urban = opts().highway_preset(HighwayPreset::UrbanUsNyc);
    let (nyc, nyc_report) = import_osm(MANHATTAN, &urban).expect("the urban preset imports");
    println!(
        "driving lanes above 90 km/h: {} under sumo-german, {} under urban-us-nyc",
        over_90(&world),
        over_90(&nyc)
    );
    // The preset changes the fallback value, not whether a way carries a tag: the tagged
    // share is the same either way. (The lane counts differ a little, because two
    // segments may only be merged when they agree on speed, so a preset that gives two
    // classes the same limit collapses a few more junctions.)
    assert!((1_500..1_550).contains(&nyc_report.counts.speeds_tagged));
    assert!((1_500..1_550).contains(&report.counts.speeds_tagged));
    assert_eq!(
        over_90(&nyc),
        0,
        "no street in New York City has a 90 km/h default"
    );
    assert!(
        over_90(&world) > 300,
        "the German design speeds are what they are, and are now named as such"
    );
    println!(
        "widths: {} tagged, {} defaulted",
        report.counts.widths_tagged, report.counts.widths_defaulted
    );
    println!(
        "short driving lanes under {} m: {}",
        report.min_useful_lane_m, report.counts.short_driving_lanes
    );
    println!(
        "junctions: {} total, {} with a drivable arm, {} with 3+ ({} signalised)",
        report.counts.junctions,
        report.counts.road_junctions,
        report.counts.major_road_junctions,
        report.counts.signalised_major_junctions
    );
    assert_eq!(
        road_junction_ids(&world).len() as u64,
        report.counts.road_junctions
    );
    assert!(
        report.counts.road_junctions < report.counts.junctions,
        "the sidewalk mesh contributes footway-only junctions"
    );
}

/// V1 (high regression), on the real extract (D7). The named towers of Midtown are as
/// tall as they are on the ground.
///
/// Folding a part into its outline without folding its HEIGHT in left the Chrysler
/// Building 10 m tall, 476 buildings materially too short and half of Midtown's built
/// volume missing. These are the ones the review named; their heights come from the
/// `building:part` volumes inside each outline, with a spire or mast cut back to its
/// structural top (V10) — which is why the Chrysler Building is its 279 m crown and not
/// its 319 m mast tip.
#[test]
#[ignore = "needs worlds/cache/manhattan.osm.xml, which is gitignored and 30 MB"]
fn midtown_landmarks_are_as_tall_as_the_city() {
    let (world, report) = import_osm(MANHATTAN, &opts()).expect("Manhattan imports");
    let height_of = |name: &str| -> f64 {
        world
            .buildings
            .iter()
            .filter(|b| b.name.map(|n| world.symbols.resolve(n)) == Some(name))
            .map(|b| b.height_m)
            .fold(0.0f64, f64::max)
    };
    // (name, metres). Every one of these was 10.0 m — the land-use default — or 22.0 m
    // before the fold, because its own outline states no height.
    for (name, expected) in [
        ("Empire State Building", 443.2),
        ("Chrysler Building", 279.0),
        ("Citigroup Center", 279.0),
        ("Bloomberg Tower", 285.0),
        ("30 Rockefeller Plaza", 260.0),
        ("4 Times Square", 252.0),
        ("The New York Times Building", 244.0),
        ("Exxon Building", 229.0),
        ("AXA Equitable Center", 229.0),
        ("One Worldwide Plaza", 195.0),
    ] {
        let got = height_of(name);
        println!("{name}: {got:.1} m");
        assert!(
            (got - expected).abs() < 0.5,
            "{name} is {got} m, expected {expected} m"
        );
    }

    // The volume is back, and with it the tall-building population an obstacle model
    // needs: 55 buildings of 150 m or more and 0.09 km^3 of prism before the fold,
    // against a Midtown that is twice that.
    let area = |b: &v2xw_world::Building| v2xw_world::ring_signed_area_2x(&b.footprint).abs() * 0.5;
    let volume: f64 = world.buildings.iter().map(|b| area(b) * b.height_m).sum();
    let tall = world
        .buildings
        .iter()
        .filter(|b| b.height_m >= 150.0)
        .count();
    println!(
        "built volume {:.4} km^3 over {} buildings, {tall} of them 150 m or taller",
        volume / 1e9,
        world.buildings.len()
    );
    assert!(volume > 0.15e9, "built volume {volume} m^3");
    assert!(tall > 120, "buildings 150 m or taller: {tall}");
    assert!(report.counts.heights_from_parts > 300, "{}", report.to_text());

    // V10: and no thin mast is left standing as solid building mass. The tallest thing in
    // the world was a 55 m^2, 443.2 m prism — the Empire State Building's spire.
    for b in &world.buildings {
        assert!(
            !(area(b) < 100.0 && b.height_m > 200.0),
            "a {:.0} m^2 prism {} m tall survived as a building",
            area(b),
            b.height_m
        );
    }
}

/// V3 and V5, on the real extract: the Phase 1 bounding box fixes the frame and bounds the
/// extent, and neither depends on what the extract happens to overhang by.
#[test]
#[ignore = "needs worlds/cache/manhattan.osm.xml, which is gitignored and 30 MB"]
fn manhattan_with_the_phase_1_box_is_bounded_and_reproducible() {
    // The D7 preset box: -73.9900, 40.7440 to -73.9680, 40.7620.
    let bbox = v2xw_world::GeoBbox::new(40.7440, -73.9900, 40.7620, -73.9680);
    let options = OsmOptions {
        bbox: Some(bbox),
        ..opts()
    };
    let (world, report) = import_osm(MANHATTAN, &options).expect("Manhattan imports");
    println!("{}", report.to_text());
    println!(
        "content hash {}",
        v2xw_world::hash::content_hash_hex(&world)
    );

    // V3. The origin is the requested south-west corner, on the 1e-7 degree grid.
    assert_eq!(report.frame.label(), "requested-bbox");
    assert_eq!(world.origin.lat_deg, 40.744);
    assert_eq!(world.origin.lon_deg, -73.99);

    // V5. The extent is the request plus one margin on each side, not 2.51x the request.
    let margin = report.bbox_clip.margin_m();
    let (east, north) = report.extent_m.expect("the extent is measured");
    let projection = v2xw_world::Projection::new(world.origin);
    let requested_east =
        (bbox.max_lon_deg - bbox.min_lon_deg) * projection.metres_per_degree_longitude();
    let requested_north =
        (bbox.max_lat_deg - bbox.min_lat_deg) * projection.metres_per_degree_latitude();
    println!(
        "extent {east:.0} x {north:.0} m against a requested {requested_east:.0} x \
         {requested_north:.0} m, margin {margin:.0} m"
    );
    assert!(
        east <= requested_east + 2.0 * margin + 1.0,
        "east extent {east} exceeds the request plus two margins"
    );
    assert!(
        north <= requested_north + 2.0 * margin + 1.0,
        "north extent {north} exceeds the request plus two margins"
    );
    println!(
        "clipped {} ways into {} runs",
        report.counts.ways_clipped, report.counts.clipped_runs
    );
    assert!(report.counts.ways_clipped > 0);

    // The whole point: two imports of the same request give the same bytes.
    let (again, _) = import_osm(MANHATTAN, &options).expect("Manhattan imports twice");
    assert_eq!(world.content_hash, again.content_hash);
    world.validate().expect("the clipped world is valid");
    for lane in world.roads.lanes() {
        assert!(!self_intersects(&lane.centreline), "lane {}", lane.id);
    }
}
