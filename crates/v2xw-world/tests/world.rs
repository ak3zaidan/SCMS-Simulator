//! End-to-end tests for `v2xw-world`, over the public API only.
//!
//! Everything is built by the procedural grid generator, because it is the one world
//! source that exists yet and because it exercises every part of the model: internal
//! lanes, conflict matrices, signal plans, rings, crossings and sites.

use v2xw_core::geom::{LanePos, Vec3};
use v2xw_core::ids::{JunctionId, LaneId};
use v2xw_world::procedural::{GridParams, GridSource, MODEL_ID, grid};
use v2xw_world::quant::{Q_HEIGHT_M, Q_POSITION_M, is_on_grid, quantise};
use v2xw_world::{
    ClassMask, ImportOptions, IndexOptions, LaneKind, World, WorldSource, WorldSourceSpec,
    serde_native, serde_vwp,
};

/// The options every test uses: a fixed import date, so nothing reads a clock.
fn opts() -> ImportOptions {
    ImportOptions::default().imported_at("2026-09-18T00:00:00Z")
}

fn small_grid() -> World {
    grid(
        &GridParams::legacy().with_size(3, 3).with_block_m(120.0),
        &opts(),
    )
    .expect("the 3 x 3 legacy grid builds")
}

fn rich_grid() -> World {
    let mut p = GridParams::tr36885_urban();
    p.rsu_at_junctions = true;
    grid(&p, &opts()).expect("the tr36885-urban grid builds")
}

// ---------------------------------------------------------------------------
// Structure
// ---------------------------------------------------------------------------

/// A 3 × 3 lattice with one lane per direction has counts that can be worked out by hand,
/// which is the point: the generator is checked against arithmetic, not against itself.
///
/// * junctions: 3 · 3 = 9
/// * street edges: two per adjacent pair, 2 · ((3−1)·3 + 3·(3−1)) = 24, one lane each
/// * movements: a corner junction has 2, an edge junction 2n + 4 = 6, the centre
///   4(n + 2) = 12, so 4·2 + 4·6 + 1·12 = 44 — and one internal lane per movement
/// * edges: 24 street edges plus one internal edge per junction = 33
/// * connections: two per movement (the abstract hop and the connector's own) = 88
#[test]
fn grid_counts_match_the_arithmetic() {
    let world = small_grid();
    let counts = world.counts();
    assert_eq!(counts.junctions, 9);
    assert_eq!(counts.edges, 33);
    assert_eq!(counts.lanes, 68);
    assert_eq!(counts.connections, 88);
    assert_eq!(counts.crossings, 0);

    let internal = world
        .roads
        .lanes()
        .iter()
        .filter(|l| l.kind == LaneKind::Internal)
        .count();
    assert_eq!(internal, 44);
    assert_eq!(counts.lanes - internal, 24);

    // Two lanes per direction: a corner still has 2 movements, an edge junction
    // 2n + 4 = 8, the centre 4(n + 2) = 16 → 4·2 + 4·8 + 16 = 56.
    let two_lane = grid(
        &GridParams::legacy().with_size(3, 3).with_block_m(120.0),
        &opts(),
    );
    assert!(two_lane.is_ok());
    let mut p = GridParams::legacy().with_size(3, 3).with_block_m(120.0);
    p.lanes_per_direction = 2;
    let world2 = grid(&p, &opts()).expect("two lanes per direction");
    let internal2 = world2
        .roads
        .lanes()
        .iter()
        .filter(|l| l.kind == LaneKind::Internal)
        .count();
    assert_eq!(internal2, 56);
    assert_eq!(world2.counts().lanes, 24 * 2 + 56);
}

#[test]
fn every_internal_lane_belongs_to_its_junction_and_matrix() {
    let world = rich_grid();
    for j in world.roads.junctions() {
        assert_eq!(
            j.conflicts.len(),
            j.internal.len(),
            "junction {} matrix and internal lane list must agree",
            j.id
        );
        for l in &j.internal {
            let lane = world.lane(*l);
            assert_eq!(lane.kind, LaneKind::Internal);
            assert_eq!(lane.junction, Some(j.id));
            assert!(world.roads.edge(lane.edge).is_internal());
        }
        for l in &j.incoming {
            assert_eq!(world.lane(*l).kind, LaneKind::Driving);
        }
    }
}

#[test]
fn signal_plans_cover_their_cycle_and_control_every_movement() {
    let world = rich_grid();
    assert_eq!(world.signals.len(), world.counts().junctions);
    for plan in &world.signals {
        let junction = world.junction(plan.junction);
        assert_eq!(plan.controlled, junction.internal);
        assert!((plan.total_phase_duration_s() - plan.cycle_s).abs() < 1e-9);
        assert_eq!(plan.phases.len(), 4);
        // Every movement is green at some point in the cycle, and never green at the
        // same time as a movement from a crossing street.
        for (i, _) in plan.controlled.iter().enumerate() {
            assert!(
                plan.phases.iter().any(|ph| ph.states[i].permits_entry()),
                "movement {i} of plan {} never gets a green",
                plan.id
            );
        }
        assert!(matches!(
            junction.control,
            v2xw_world::JunctionControl::Signalised { plan: p } if p == plan.id
        ));
        // A head for every approach lane that has a movement.
        assert!(!plan.heads.is_empty());
        for head in &plan.heads {
            assert!(junction.incoming.contains(&head.lane));
        }
    }
}

#[test]
fn conflict_matrix_is_symmetric_and_response_is_antisymmetric() {
    let world = rich_grid();
    let mut conflicts = 0;
    for j in world.roads.junctions() {
        let n = j.conflicts.len();
        for a in 0..n {
            assert!(
                !j.conflicts.is_foe(a, a),
                "a movement cannot be its own foe"
            );
            for b in 0..n {
                assert_eq!(j.conflicts.is_foe(a, b), j.conflicts.is_foe(b, a));
                if j.conflicts.must_yield(a, b) {
                    conflicts += 1;
                    assert!(j.conflicts.is_foe(a, b), "yielding implies conflicting");
                    assert!(
                        !j.conflicts.must_yield(b, a),
                        "two movements cannot both give way to each other"
                    );
                }
            }
        }
    }
    assert!(conflicts > 0, "an urban grid has conflicting movements");
}

// ---------------------------------------------------------------------------
// Lane geometry
// ---------------------------------------------------------------------------

#[test]
fn cumulative_arc_length_is_monotonic_and_matches_the_centreline() {
    for world in [small_grid(), rich_grid()] {
        for lane in world.roads.lanes() {
            assert_eq!(lane.cumulative.len(), lane.centreline.len());
            assert_eq!(lane.cumulative[0], 0.0);
            assert_eq!(lane.length_m, *lane.cumulative.last().unwrap());
            let recomputed = lane.recompute_cumulative();
            for (i, (stored, want)) in lane.cumulative.iter().zip(&recomputed).enumerate() {
                assert!(
                    (stored - want).abs() <= Q_POSITION_M * 0.5,
                    "lane {} cumulative[{i}]: stored {stored}, centreline gives {want}",
                    lane.id
                );
                if i > 0 {
                    assert!(
                        *stored > lane.cumulative[i - 1],
                        "lane {} arc length must strictly increase",
                        lane.id
                    );
                }
            }
        }
    }
}

#[test]
fn project_then_to_xyz_round_trips_within_the_quantisation_grid() {
    let world = rich_grid();
    let mut checked = 0;
    let mut clamped = 0;
    // Sample every lane at a few arc lengths, on and off the centreline. `project` finds
    // the nearest lane, which at a junction may be a *different* lane from the one the
    // point was generated on — the reconstruction still has to land back on the point,
    // because the lateral offset is signed and measured from that lane.
    for lane in world.roads.lanes() {
        for fraction in [0.1, 0.3, 0.5, 0.7, 0.9] {
            for offset in [0.0, 0.4, -0.4] {
                let s = lane.length_m * fraction;
                let point = lane.offset_point(s, offset);
                let back = world
                    .project(point)
                    .unwrap_or_else(|| panic!("lane {} at s = {s} projects", lane.id));
                // A projection clamped to a lane's end is not a round trip: the point is
                // beyond the polyline, so the nearest point on it cannot reproduce the
                // query. Those are counted, not asserted on.
                let hit = world.lane(back.lane);
                if back.s_m <= Q_POSITION_M || back.s_m >= hit.length_m - Q_POSITION_M {
                    clamped += 1;
                    continue;
                }
                let xyz = world.to_xyz(&back);
                let error = (xyz - point).norm_2d();
                assert!(
                    error <= 4.0 * Q_POSITION_M,
                    "lane {} at s = {s}, d = {offset}: projected to {back:?}, which maps \
                     back {error} m away",
                    lane.id
                );
                checked += 1;
            }
        }
    }
    assert!(
        checked > 10 * clamped,
        "{clamped} of {} samples were clamped to a lane end; the test would not be \
         checking much",
        checked + clamped
    );
}

#[test]
fn projection_ties_break_by_lane_id() {
    let world = small_grid();
    // The centre of a street is exactly equidistant from its east-bound and west-bound
    // lanes. The east-bound edge is created first, so it holds the lower lane id, and
    // 03-interfaces §2 requires that one to win.
    let j0 = world.junction(JunctionId::new(0)).position;
    let j1 = world.junction(JunctionId::new(1)).position;
    let midpoint = Vec3::new_2d((j0.x + j1.x) * 0.5, j0.y);
    let hit = world.project(midpoint).expect("a lane is near the street");
    let other = world
        .roads
        .lanes()
        .iter()
        .filter(|l| l.kind == LaneKind::Driving)
        .map(|l| (l.id, l.project_point(midpoint).distance_m))
        .filter(|(_, d)| (*d - 1.75).abs() < 1e-12)
        .collect::<Vec<_>>();
    assert_eq!(other.len(), 2, "exactly two lanes are 1.75 m away");
    assert_eq!(hit.lane, other.iter().map(|(id, _)| *id).min().unwrap());
}

#[test]
fn nearest_lane_respects_the_radius_and_the_class_filter() {
    let world = small_grid();
    let start = world.lane(LaneId::new(0)).start();
    assert!(world.nearest_lane_within(start, 1.0, None).is_some());

    // A point far outside the world: found with an unbounded search, not within 10 m.
    let far = Vec3::new_2d(-5_000.0, -5_000.0);
    assert!(world.nearest_lane_within(far, 10.0, None).is_none());
    assert!(world.project(far).is_some());

    // The grid generates motor-traffic lanes only, so a rail query finds nothing.
    assert!(
        world
            .nearest_lane_within(start, 100.0, Some(ClassMask::RAIL))
            .is_none()
    );
    assert!(
        world
            .nearest_lane_within(start, 100.0, Some(ClassMask::CAR))
            .is_some()
    );
}

#[test]
fn grid_cell_size_does_not_change_the_answer() {
    let mut coarse_opts = opts();
    coarse_opts.index_options = IndexOptions {
        lane_grid_cell_m: 7.0,
        ..IndexOptions::default()
    };
    let params = GridParams::legacy().with_size(3, 3);
    let fine = grid(&params, &opts()).unwrap();
    let coarse = grid(&params, &coarse_opts).unwrap();
    assert_eq!(fine.content_hash, coarse.content_hash);

    for i in 0..40 {
        for j in 0..40 {
            let p = Vec3::new_2d(f64::from(i) * 9.7, f64::from(j) * 11.3);
            assert_eq!(
                fine.project(p),
                coarse.project(p),
                "the grid cell size is a performance knob, not an answer"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Connectivity
// ---------------------------------------------------------------------------

#[test]
fn successors_form_a_connected_graph() {
    let world = grid(&GridParams::legacy().with_size(4, 4), &opts()).unwrap();
    let total = world.counts().lanes;

    // Forwards from lane 0.
    let mut seen = vec![false; total];
    let mut stack = vec![LaneId::new(0)];
    seen[0] = true;
    while let Some(lane) = stack.pop() {
        for next in world.successor_lanes(lane) {
            if !seen[next.as_usize()] {
                seen[next.as_usize()] = true;
                stack.push(next);
            }
        }
    }
    assert!(
        seen.iter().all(|s| *s),
        "with one lane per direction every lane is reachable from every other: {} of {} \
         were not",
        seen.iter().filter(|s| !**s).count(),
        total
    );

    // Backwards too, which makes the graph strongly connected.
    let mut seen_back = vec![false; total];
    let mut stack = vec![LaneId::new(0)];
    seen_back[0] = true;
    while let Some(lane) = stack.pop() {
        for c in world.predecessors(lane) {
            let previous = c.via.unwrap_or(c.from_lane);
            for candidate in [previous, c.from_lane] {
                if !seen_back[candidate.as_usize()] {
                    seen_back[candidate.as_usize()] = true;
                    stack.push(candidate);
                }
            }
        }
    }
    assert!(seen_back.iter().all(|s| *s));
}

#[test]
fn every_movement_is_reachable_through_its_connector() {
    let world = small_grid();
    for j in world.roads.junctions() {
        for internal in &j.internal {
            let entries: Vec<_> = world
                .predecessors(*internal)
                .iter()
                .chain(
                    world
                        .roads
                        .connections()
                        .iter()
                        .filter(|c| c.via == Some(*internal)),
                )
                .collect();
            assert!(
                !entries.is_empty(),
                "internal lane {internal} of junction {} has no way in",
                j.id
            );
            // And it leads somewhere.
            assert_eq!(world.successors(*internal).len(), 1);
        }
    }
}

// ---------------------------------------------------------------------------
// Content hash
// ---------------------------------------------------------------------------

#[test]
fn content_hash_is_stable_and_sensitive() {
    let a = small_grid();
    let b = small_grid();
    assert_eq!(a.content_hash, b.content_hash);
    assert_eq!(a.provenance.content_hash, a.content_hash);

    // A different import date is not a different world.
    let dated = grid(
        &GridParams::legacy().with_size(3, 3).with_block_m(120.0),
        &ImportOptions::default().imported_at("1999-01-01T00:00:00Z"),
    )
    .unwrap();
    assert_eq!(
        dated.content_hash, a.content_hash,
        "the content hash covers geometry, not the day it was imported"
    );

    // A millimetre of geometry is.
    let moved = grid(
        &GridParams::legacy().with_size(3, 3).with_block_m(120.001),
        &opts(),
    )
    .unwrap();
    assert_ne!(moved.content_hash, a.content_hash);

    // So is a lattice of a different size, a different lane count, or signals.
    for params in [
        GridParams::legacy().with_size(4, 3),
        {
            let mut p = GridParams::legacy().with_size(3, 3);
            p.lanes_per_direction = 2;
            p
        },
        GridParams::legacy().with_size(3, 3).with_signals(true),
    ] {
        let other = grid(&params, &opts()).unwrap();
        assert_ne!(other.content_hash, a.content_hash);
    }
}

#[test]
fn content_hash_survives_a_round_trip_and_catches_tampering() {
    let world = rich_grid();
    let bytes = serde_native::to_bytes(&world).unwrap();
    let back = serde_native::from_bytes(&bytes).unwrap();
    assert_eq!(back.content_hash, world.content_hash);

    // Move one building a millimetre in the serialised JSON and the reader must refuse
    // it, because the stored hash no longer matches the geometry (invariant I-W2).
    let json = serde_native::to_json(&world).unwrap();
    let tampered = json.replacen("\"height_m\":20.0", "\"height_m\":20.001", 1);
    assert_ne!(tampered, json, "the test must actually change something");
    let err = serde_native::from_json(&tampered).expect_err("tampering is caught");
    assert!(err.to_string().contains("I-W2"), "unexpected error: {err}");
}

// ---------------------------------------------------------------------------
// Native serialisation
// ---------------------------------------------------------------------------

#[test]
fn native_round_trip_is_exact() {
    for world in [small_grid(), rich_grid()] {
        let bytes = serde_native::to_bytes(&world).unwrap();
        let back = serde_native::from_bytes(&bytes).unwrap();
        assert_eq!(back, world, "the binary round trip must be exact");
        assert_eq!(serde_native::to_bytes(&back).unwrap(), bytes);

        let json = serde_native::to_json(&world).unwrap();
        let from_json = serde_native::from_json(&json).unwrap();
        assert_eq!(from_json, world, "the JSON round trip must be exact");
        assert_eq!(serde_native::to_json(&from_json).unwrap(), json);
    }
}

#[test]
fn native_writer_is_byte_stable() {
    let world = rich_grid();
    assert_eq!(
        serde_native::to_bytes(&world).unwrap(),
        serde_native::to_bytes(&world).unwrap()
    );
    let bytes = serde_native::to_bytes(&world).unwrap();
    assert_eq!(&bytes[..8], b"V2XWWRLD");
}

#[test]
fn native_reader_rejects_rubbish() {
    assert!(serde_native::from_bytes(b"").is_err());
    assert!(serde_native::from_bytes(b"not a world file at all").is_err());
    let mut bytes = serde_native::to_bytes(&small_grid()).unwrap();
    bytes[8] = 9; // format version
    assert!(serde_native::from_bytes(&bytes).is_err());
}

// ---------------------------------------------------------------------------
// The `vwp-world/1` payload
// ---------------------------------------------------------------------------

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn f32_at(b: &[u8], at: usize) -> f32 {
    f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn f64_at(b: &[u8], at: usize) -> f64 {
    f64::from_le_bytes([
        b[at],
        b[at + 1],
        b[at + 2],
        b[at + 3],
        b[at + 4],
        b[at + 5],
        b[at + 6],
        b[at + 7],
    ])
}

/// Every `f32` the payload's geometry columns hold, with the directory around them.
struct Decoded {
    body: Vec<u8>,
    lane_count: usize,
    lane_point_total: usize,
    building_count: usize,
    ring_point_total: usize,
    off_lanes: usize,
    off_lane_points: usize,
    off_buildings: usize,
    off_ring_points: usize,
    junction_count: usize,
    off_junctions: usize,
    signal_count: usize,
    off_signals: usize,
    site_count: usize,
    off_sites: usize,
    off_strings: usize,
    crossing_count: usize,
    off_crossings: usize,
    landuse_count: usize,
    off_landuse: usize,
    off_provenance: usize,
    provenance_bytes: usize,
}

/// A minimal independent reader for the payload — deliberately written from the
/// specification's tables rather than from the writer, so that the two have to agree.
fn decode(file: &[u8]) -> Decoded {
    assert_eq!(u32_at(file, 0), 0x444C_5756, "magic is V W L D");
    assert_eq!(u16_at(file, 4), 1, "version");
    assert_eq!(u16_at(file, 6), 0, "reserved");
    assert_eq!(u16_at(file, 12), 0, "flags: uncompressed");
    let body_len = u32_at(file, 8) as usize;
    assert_eq!(file.len(), 16 + body_len);
    let body = file[16..].to_vec();
    let u = |at: usize| u32_at(&body, at) as usize;
    Decoded {
        lane_count: u(96),
        lane_point_total: u(100),
        building_count: u(104),
        ring_point_total: u(108),
        off_lanes: u(112),
        off_lane_points: u(116),
        off_buildings: u(120),
        off_ring_points: u(124),
        junction_count: u(128),
        off_junctions: u(132),
        signal_count: u(136),
        off_signals: u(140),
        site_count: u(144),
        off_sites: u(148),
        off_strings: u(152),
        crossing_count: u(156),
        off_crossings: u(160),
        landuse_count: u(164),
        off_landuse: u(168),
        off_provenance: u(172),
        provenance_bytes: u(176),
        body,
    }
}

#[test]
fn vwp_payload_is_byte_identical_for_the_same_world() {
    let world = rich_grid();
    let a = serde_vwp::write(&world).unwrap();
    let b = serde_vwp::write(&world).unwrap();
    assert_eq!(a.bytes, b.bytes);
    assert_eq!(a.content_hash, b.content_hash);
    assert!(a.precision_warnings.is_empty());

    // And for a world that went through the engine's own format and back.
    let round_tripped = serde_native::from_bytes(&serde_native::to_bytes(&world).unwrap()).unwrap();
    assert_eq!(serde_vwp::write(&round_tripped).unwrap().bytes, a.bytes);
}

#[test]
fn vwp_payload_hash_verifies() {
    let world = rich_grid();
    let payload = serde_vwp::write(&world).unwrap();
    assert_eq!(
        serde_vwp::stored_content_hash(&payload.bytes).unwrap(),
        payload.content_hash,
        "the hash in the directory is the hash of the body"
    );
    assert_eq!(
        serde_vwp::verify(&payload.bytes).unwrap(),
        payload.content_hash,
        "a reader recomputing the hash gets the same answer (conformance W1/W3)"
    );
    assert!(payload.url_path().starts_with("/world/"));
    assert!(payload.url_path().ends_with(".vwb"));
    assert_eq!(payload.content_hash_hex().len(), 64);

    // Flip one geometry byte and the check must fail.
    let mut tampered = payload.bytes.clone();
    let at = 16 + 192 + 4;
    tampered[at] ^= 0x01;
    assert_ne!(serde_vwp::verify(&tampered).unwrap(), payload.content_hash);
}

#[test]
fn vwp_payload_matches_the_specification_layout() {
    let world = rich_grid();
    let payload = serde_vwp::write(&world).unwrap();
    let d = decode(&payload.bytes);

    assert_eq!(d.lane_count, world.counts().lanes);
    assert_eq!(
        d.lane_point_total,
        world
            .roads
            .lanes()
            .iter()
            .map(|l| l.centreline.len())
            .sum::<usize>()
    );
    assert_eq!(d.building_count, world.buildings.len());
    assert_eq!(d.junction_count, world.counts().junctions);
    assert_eq!(
        d.signal_count,
        world.signals.iter().map(|p| p.heads.len()).sum::<usize>()
    );
    assert_eq!(d.site_count, world.sites.len());
    assert_eq!(d.crossing_count, world.counts().crossings);
    assert_eq!(d.landuse_count, world.landuse.len());
    assert!(d.off_provenance > 0 && d.provenance_bytes > 0);

    // §4.2 — the directory's f64 fields.
    assert_eq!(f64_at(&d.body, 32), world.origin.lat_deg);
    assert_eq!(f64_at(&d.body, 40), world.origin.lon_deg);
    assert_eq!(f64_at(&d.body, 48), world.origin.alt_m);
    assert_eq!(f64_at(&d.body, 56), world.bbox.min.x);
    assert_eq!(f64_at(&d.body, 64), world.bbox.min.y);
    assert_eq!(f64_at(&d.body, 72), world.bbox.max.x);
    assert_eq!(f64_at(&d.body, 80), world.bbox.max.y);

    // §4.3 — the lane table, column by column.
    let l = d.lane_count;
    let mut expected_point_off = 0usize;
    for (i, lane) in world.roads.lanes().iter().enumerate() {
        assert_eq!(u32_at(&d.body, d.off_lanes + 4 * i), lane.id.index());
        assert_eq!(
            u32_at(&d.body, d.off_lanes + 4 * l + 4 * i) as usize,
            expected_point_off
        );
        assert_eq!(
            u32_at(&d.body, d.off_lanes + 8 * l + 4 * i) as usize,
            lane.centreline.len()
        );
        assert!(
            u32_at(&d.body, d.off_lanes + 8 * l + 4 * i) >= 2,
            "§4.3: ≥ 2 points"
        );
        assert_eq!(
            u32_at(&d.body, d.off_lanes + 12 * l + 4 * i),
            lane.edge.index()
        );
        assert_eq!(
            u32_at(&d.body, d.off_lanes + 16 * l + 4 * i),
            lane.junction.map_or(0xFFFF_FFFF, |j| j.index()),
        );
        assert_eq!(
            f32_at(&d.body, d.off_lanes + 24 * l + 4 * i),
            lane.width_m as f32
        );
        assert_eq!(
            u16_at(&d.body, d.off_lanes + 32 * l + 2 * i),
            lane.allowed.bits()
        );
        assert_eq!(d.body[d.off_lanes + 34 * l + i], lane.kind.wire_code());
        assert_eq!(d.body[d.off_lanes + 35 * l + i], lane.index);

        // The centreline, in the three parallel f32 arrays, in travel order.
        let t = d.lane_point_total;
        for (k, p) in lane.centreline.iter().enumerate() {
            let at = expected_point_off + k;
            assert_eq!(f32_at(&d.body, d.off_lane_points + 4 * at), p.x as f32);
            assert_eq!(
                f32_at(&d.body, d.off_lane_points + 4 * t + 4 * at),
                p.y as f32
            );
            assert_eq!(
                f32_at(&d.body, d.off_lane_points + 8 * t + 4 * at),
                p.z as f32
            );
        }
        expected_point_off += lane.centreline.len();
    }

    // §4.4 — buildings, and the rings they share with land use.
    let b = d.building_count;
    let mut ring_cursor = 0usize;
    for (i, building) in world.buildings.iter().enumerate() {
        let ring = building.open_ring();
        assert_eq!(
            u32_at(&d.body, d.off_buildings + 4 * i),
            building.id.index()
        );
        assert_eq!(
            u32_at(&d.body, d.off_buildings + 4 * b + 4 * i) as usize,
            ring_cursor
        );
        assert_eq!(
            u32_at(&d.body, d.off_buildings + 8 * b + 4 * i) as usize,
            ring.len()
        );
        assert!(ring.len() >= 3, "§4.4: a ring has at least 3 points");
        assert_eq!(
            f32_at(&d.body, d.off_buildings + 12 * b + 4 * i),
            building.height_m as f32
        );
        assert_eq!(
            u16_at(&d.body, d.off_buildings + 26 * b + 2 * i),
            building.levels.unwrap_or(0xFFFF)
        );
        for (k, p) in ring.iter().enumerate() {
            let at = ring_cursor + k;
            assert_eq!(f32_at(&d.body, d.off_ring_points + 4 * at), p.x as f32);
            assert_eq!(
                f32_at(&d.body, d.off_ring_points + 4 * d.ring_point_total + 4 * at),
                p.y as f32
            );
        }
        ring_cursor += ring.len();
    }
    // Land-use rings follow the building rings in the same arrays (§4.4).
    for (i, zone) in world.landuse.iter().enumerate() {
        let at = d.off_landuse + 16 * i;
        assert_eq!(u32_at(&d.body, at), zone.id.index());
        assert_eq!(u32_at(&d.body, at + 4) as usize, ring_cursor);
        assert_eq!(u32_at(&d.body, at + 8) as usize, zone.open_ring().len());
        assert_eq!(d.body[at + 12], zone.class.wire_code());
        ring_cursor += zone.open_ring().len();
    }
    assert_eq!(ring_cursor, d.ring_point_total);

    // §4.5 — junctions, signal heads, sites, crossings.
    for (i, j) in world.roads.junctions().iter().enumerate() {
        let at = d.off_junctions + 24 * i;
        assert_eq!(u32_at(&d.body, at), j.id.index());
        assert_eq!(f32_at(&d.body, at + 8), j.position.x as f32);
        assert_eq!(f32_at(&d.body, at + 12), j.position.y as f32);
        assert_eq!(d.body[at + 20], j.control.wire_code());
    }
    let mut head_index = 0;
    for plan in &world.signals {
        for head in &plan.heads {
            let at = d.off_signals + 28 * head_index;
            assert_eq!(u32_at(&d.body, at), plan.id.index());
            assert_eq!(u32_at(&d.body, at + 4), plan.junction.index());
            assert_eq!(u32_at(&d.body, at + 8), head.lane.index());
            assert_eq!(f32_at(&d.body, at + 20), head.position.z as f32);
            assert_eq!(d.body[at + 24], head.kind.wire_code());
            assert_eq!(u16_at(&d.body, at + 26), head.group);
            head_index += 1;
        }
    }
    for (i, s) in world.sites.iter().enumerate() {
        let at = d.off_sites + 32 * i;
        assert_eq!(u32_at(&d.body, at), s.id.index());
        assert_eq!(u32_at(&d.body, at + 4), 0xFFFF_FFFF, "no node assigned yet");
        assert_eq!(f32_at(&d.body, at + 20), s.antenna_height_m as f32);
        assert_eq!(d.body[at + 28], s.kind.wire_code());
    }
    for (i, c) in world.crossings().iter().enumerate() {
        let at = d.off_crossings + 28 * i;
        assert_eq!(u32_at(&d.body, at), c.id.index());
        assert_eq!(u32_at(&d.body, at + 4), c.junction.index());
        assert_eq!(f32_at(&d.body, at + 24), c.width_m as f32);
    }
}

#[test]
fn vwp_symbol_table_decodes() {
    let world = rich_grid();
    let payload = serde_vwp::write(&world).unwrap();
    let d = decode(&payload.bytes);
    let at = d.off_strings;
    let n = u32_at(&d.body, at) as usize;
    let blob_bytes = u32_at(&d.body, at + 4) as usize;
    assert_eq!(n, world.symbols.len());
    let offsets: Vec<usize> = (0..=n)
        .map(|i| u32_at(&d.body, at + 8 + 4 * i) as usize)
        .collect();
    assert_eq!(offsets[0], 0);
    assert_eq!(offsets[n], blob_bytes);
    let blob_at = at + 8 + 4 * (n + 1);
    for i in 0..n {
        let text = core::str::from_utf8(&d.body[blob_at + offsets[i]..blob_at + offsets[i + 1]])
            .expect("UTF-8");
        assert_eq!(text, world.symbols.strings()[i]);
    }
    assert_eq!(
        world.symbols.strings()[0],
        "",
        "§2.5: id 0 is the empty string"
    );
}

#[test]
fn vwp_json_mirrors_the_binary() {
    let world = rich_grid();
    let payload = serde_vwp::write(&world).unwrap();
    let json = serde_vwp::to_json(&world).unwrap();

    assert_eq!(json["schema"], "vwp-world/1");
    assert_eq!(json["content_hash"], payload.content_hash_hex());
    assert_eq!(
        json["lanes"].as_array().unwrap().len(),
        world.counts().lanes
    );
    assert_eq!(
        json["buildings"].as_array().unwrap().len(),
        world.buildings.len()
    );
    assert_eq!(
        json["junctions"].as_array().unwrap().len(),
        world.counts().junctions
    );
    assert_eq!(
        json["signals"].as_array().unwrap().len(),
        world.signals.iter().map(|p| p.heads.len()).sum::<usize>()
    );
    assert_eq!(json["sites"].as_array().unwrap().len(), world.sites.len());
    assert_eq!(
        json["crossings"].as_array().unwrap().len(),
        world.counts().crossings
    );

    // Conformance W4: the same lanes, with the same numbers, in the same order.
    let d = decode(&payload.bytes);
    for (i, lane_json) in json["lanes"].as_array().unwrap().iter().enumerate() {
        assert_eq!(
            lane_json["lane_id"].as_u64().unwrap() as u32,
            u32_at(&d.body, d.off_lanes + 4 * i)
        );
        let centreline = lane_json["centreline"].as_array().unwrap();
        let point_off = u32_at(&d.body, d.off_lanes + 4 * d.lane_count + 4 * i) as usize;
        let point_count = u32_at(&d.body, d.off_lanes + 8 * d.lane_count + 4 * i) as usize;
        assert_eq!(centreline.len(), point_count * 3);
        for k in 0..point_count {
            let x = f32_at(&d.body, d.off_lane_points + 4 * (point_off + k));
            assert_eq!(centreline[k * 3].as_f64().unwrap(), f64::from(x));
        }
    }
    // The provenance travels in both forms.
    assert_eq!(json["provenance"]["source"], "procedural");
    assert!(
        json["provenance"]["transformations"]
            .as_array()
            .unwrap()
            .len()
            >= 3
    );
    let embedded: serde_json::Value =
        serde_json::from_slice(&d.body[d.off_provenance..d.off_provenance + d.provenance_bytes])
            .unwrap();
    assert_eq!(embedded, json["provenance"]);
}

// ---------------------------------------------------------------------------
// D9 — the scanning test
// ---------------------------------------------------------------------------

/// Every float in the model is on its declared grid. This is the scan D9 requires, and it
/// runs over the field-aware visitor rather than over a blanket tolerance, so a field with
/// a different quantum (dB, degrees, seconds) is checked against *its* quantum.
#[test]
fn d9_no_model_float_is_off_its_grid() {
    for world in [small_grid(), rich_grid()] {
        let mut offenders = Vec::new();
        world.scan_exported_floats(&mut |path, value, quantum| {
            if !is_on_grid(value, quantum) {
                offenders.push(format!("{path} = {value} is not a multiple of {quantum}"));
            }
        });
        assert!(offenders.is_empty(), "off-grid floats: {offenders:#?}");
    }
}

/// Every number in the engine's own JSON form is on the finest grid any field uses.
///
/// A blanket check over the serialised text, to catch a field the field-aware visitor does
/// not know about: a raw IEEE-754 double from a `sqrt` or a `cos` would fail it
/// immediately, which is exactly the class of bug D9 exists to stop.
#[test]
fn d9_no_native_json_number_is_a_raw_double() {
    let world = rich_grid();
    let json: serde_json::Value =
        serde_json::from_str(&serde_native::to_json(&world).unwrap()).unwrap();
    let mut offenders = Vec::new();
    fn walk(v: &serde_json::Value, path: &str, offenders: &mut Vec<String>) {
        match v {
            serde_json::Value::Number(n) => {
                if let Some(x) = n.as_f64() {
                    if x.fract() != 0.0 && !is_on_grid(x, 1e-7) {
                        offenders.push(format!("{path} = {x}"));
                    }
                }
            }
            serde_json::Value::Array(items) => {
                for (i, item) in items.iter().enumerate() {
                    walk(item, &format!("{path}[{i}]"), offenders);
                }
            }
            serde_json::Value::Object(map) => {
                for (k, item) in map {
                    walk(item, &format!("{path}.{k}"), offenders);
                }
            }
            _ => {}
        }
    }
    walk(&json, "$", &mut offenders);
    assert!(
        offenders.is_empty(),
        "raw doubles in the JSON: {offenders:#?}"
    );
}

/// Every `f32` the `vwp-world/1` payload holds is the narrowing of an on-grid value, and
/// widening it again lands back on the grid — the wire-side half of D9.
#[test]
fn d9_no_payload_float_is_off_its_grid() {
    let world = rich_grid();
    let payload = serde_vwp::write(&world).unwrap();
    let d = decode(&payload.bytes);
    let mut offenders = Vec::new();
    // The D9 property for an `f32` column: the value stored is the narrowing of an
    // on-grid `f64`, so widening it, putting it back on the grid and narrowing it again
    // returns the identical `f32`. A raw double that was never quantised fails this —
    // which is the whole point of the scan.
    let mut check = |what: String, value: f32, quantum: f64| {
        let wide = f64::from(value);
        let regrid = quantise(wide, quantum);
        if v2xw_world::quant::quantise_f32(regrid, quantum) != value || !is_on_grid(regrid, quantum)
        {
            offenders.push(format!("{what} = {wide}"));
        }
    };
    let t = d.lane_point_total;
    for i in 0..t {
        check(
            format!("lane_points.x[{i}]"),
            f32_at(&d.body, d.off_lane_points + 4 * i),
            Q_POSITION_M,
        );
        check(
            format!("lane_points.y[{i}]"),
            f32_at(&d.body, d.off_lane_points + 4 * t + 4 * i),
            Q_POSITION_M,
        );
        check(
            format!("lane_points.z[{i}]"),
            f32_at(&d.body, d.off_lane_points + 8 * t + 4 * i),
            Q_HEIGHT_M,
        );
    }
    for i in 0..d.ring_point_total {
        check(
            format!("ring_points.x[{i}]"),
            f32_at(&d.body, d.off_ring_points + 4 * i),
            Q_POSITION_M,
        );
        check(
            format!("ring_points.y[{i}]"),
            f32_at(&d.body, d.off_ring_points + 4 * d.ring_point_total + 4 * i),
            Q_POSITION_M,
        );
    }
    let l = d.lane_count;
    for i in 0..l {
        check(
            format!("lanes.width_m[{i}]"),
            f32_at(&d.body, d.off_lanes + 24 * l + 4 * i),
            Q_POSITION_M,
        );
        check(
            format!("lanes.speed_limit_mps[{i}]"),
            f32_at(&d.body, d.off_lanes + 28 * l + 4 * i),
            1e-3,
        );
    }
    for i in 0..d.building_count {
        check(
            format!("buildings.height_m[{i}]"),
            f32_at(&d.body, d.off_buildings + 12 * d.building_count + 4 * i),
            Q_HEIGHT_M,
        );
        check(
            format!("buildings.base_z_m[{i}]"),
            f32_at(&d.body, d.off_buildings + 16 * d.building_count + 4 * i),
            Q_HEIGHT_M,
        );
    }
    for i in 0..d.junction_count {
        for (k, off) in [(0, 8), (1, 12), (2, 16)] {
            check(
                format!("junctions[{i}].xyz[{k}]"),
                f32_at(&d.body, d.off_junctions + 24 * i + off),
                Q_POSITION_M,
            );
        }
    }
    for i in 0..d.signal_count {
        for (k, off) in [(0, 12), (1, 16), (2, 20)] {
            check(
                format!("signals[{i}].xyz[{k}]"),
                f32_at(&d.body, d.off_signals + 28 * i + off),
                Q_POSITION_M,
            );
        }
    }
    for i in 0..d.site_count {
        for (k, off) in [(0, 8), (1, 12), (2, 16), (3, 20)] {
            check(
                format!("sites[{i}].f32[{k}]"),
                f32_at(&d.body, d.off_sites + 32 * i + off),
                Q_POSITION_M,
            );
        }
        check(
            format!("sites[{i}].antenna_gain_dbi"),
            f32_at(&d.body, d.off_sites + 32 * i + 24),
            1e-2,
        );
    }
    for i in 0..d.crossing_count {
        for (k, off) in [(0, 8), (1, 12), (2, 16), (3, 20), (4, 24)] {
            check(
                format!("crossings[{i}].f32[{k}]"),
                f32_at(&d.body, d.off_crossings + 28 * i + off),
                Q_POSITION_M,
            );
        }
    }
    assert!(
        offenders.is_empty(),
        "off-grid payload floats: {offenders:#?}"
    );
}

// ---------------------------------------------------------------------------
// Buildings and the R-tree
// ---------------------------------------------------------------------------

#[test]
fn building_index_answers_in_id_order() {
    let world = rich_grid();
    assert!(!world.buildings.is_empty());

    // Everything, by asking for the whole world.
    let all = world.buildings_in_bbox(world.bbox);
    assert_eq!(all.len(), world.buildings.len());
    assert!(
        all.windows(2).all(|w| w[0] < w[1]),
        "results are id-ordered"
    );

    // A point inside a building finds it, and only it.
    let first = &world.buildings[0];
    let inside = first
        .footprint
        .iter()
        .fold(Vec3::ZERO, |acc, p| acc + *p)
        .scale(1.0 / first.footprint.len() as f64);
    assert!(first.contains_2d(inside));
    assert_eq!(world.nearest_building(inside), Some(first.id));
    assert!(world.buildings_near(inside, 1.0).contains(&first.id));

    // A segment through a building crosses its outer ring twice.
    let bb = first.bbox();
    let a = Vec3::new_2d(bb.min.x - 50.0, inside.y);
    let b = Vec3::new_2d(bb.max.x + 50.0, inside.y);
    assert!(world.buildings_on_segment(a, b).contains(&first.id));
    assert_eq!(world.wall_crossings(first.id, a, b), 2);
    // A segment that stops inside crosses it once.
    assert_eq!(world.wall_crossings(first.id, a, inside), 1);
    // A segment along a street misses every building.
    let street = world.junction(JunctionId::new(0)).position;
    let street_end = world.junction(JunctionId::new(1)).position;
    assert!(world.buildings_on_segment(street, street_end).is_empty());
}

#[test]
fn land_use_answers_the_environment_question() {
    let world = rich_grid();
    let inside = world.bbox.center();
    assert_eq!(world.env_class_at(inside), v2xw_world::EnvClass::Urban);
    let outside = Vec3::new_2d(-1000.0, -1000.0);
    assert_eq!(world.env_class_at(outside), world.default_env);
}

#[test]
fn index_statistics_are_sane() {
    let world = rich_grid();
    let stats = world.index_stats();
    assert!(stats.grid_nx > 1 && stats.grid_ny > 1);
    assert!(stats.grid_entries >= world.counts().lanes);
    assert_eq!(stats.building_entries, world.buildings.len());
    assert_eq!(stats.predecessor_entries, world.counts().connections);
    assert_eq!(
        stats.occupancy.values().sum::<usize>(),
        (stats.grid_nx * stats.grid_ny) as usize
    );
}

// ---------------------------------------------------------------------------
// The plug-in seam
// ---------------------------------------------------------------------------

#[test]
fn world_source_builds_the_same_world_as_the_function() {
    let source = GridSource::new();
    let spec = WorldSourceSpec::procedural(
        MODEL_ID,
        serde_json::json!({"cols": 3, "rows": 3, "block_x_m": 120.0, "block_y_m": 120.0}),
    );
    let via_seam = source.build(&spec, &opts()).unwrap();
    assert_eq!(via_seam.content_hash, small_grid().content_hash);

    // An unknown source is refused, not guessed at.
    let other = WorldSourceSpec::SumoNet {
        path: "somewhere.net.xml".to_string(),
    };
    let err = source.build(&other, &opts()).unwrap_err();
    assert!(err.to_string().contains("does not support"), "{err}");

    // An unknown parameter is refused too: a typo in a scenario must not silently
    // fall back to a default.
    let typo = WorldSourceSpec::procedural(MODEL_ID, serde_json::json!({"colls": 3}));
    assert!(source.build(&typo, &opts()).is_err());
}

#[test]
fn model_card_is_valid_and_declares_every_parameter() {
    let card = GridSource::new().card();
    card.validate().expect("the card passes the registry rules");
    assert_eq!(card.id, MODEL_ID);
    assert!(
        !card.determinism.uses_rng,
        "the generator draws no randomness"
    );

    // Every parameter of GridParams is declared (invariant I-C3). The struct's field
    // names are the scenario's spelling, so serialising a default parameter set gives
    // exactly the list the card must cover.
    let params = serde_json::to_value(GridParams::legacy()).unwrap();
    for name in params.as_object().unwrap().keys() {
        assert!(
            card.parameters.iter().any(|p| &p.name == name),
            "parameter {name} is read by the generator but missing from its card"
        );
    }
    // And nothing is claimed as sourced when it is not.
    assert_eq!(card.todo_calibrate().count(), 8);
}

#[test]
fn parameter_validation_rejects_impossible_worlds() {
    let bad = [
        GridParams::legacy().with_size(1, 4),
        GridParams::legacy().with_size(4, 1),
        // A block shorter than the two junction areas it joins.
        GridParams::legacy().with_block_m(3.0),
        {
            let mut p = GridParams::legacy();
            p.lanes_per_direction = 0;
            p
        },
        {
            let mut p = GridParams::legacy();
            p.lane_width_m = 0.0;
            p
        },
        {
            let mut p = GridParams::legacy().with_signals(true);
            p.cycle_s = 4.0;
            p
        },
    ];
    for params in bad {
        assert!(
            grid(&params, &opts()).is_err(),
            "these parameters should not build: {params:?}"
        );
    }
}

#[test]
fn provenance_records_what_the_generator_did() {
    let world = rich_grid();
    let p = &world.provenance;
    assert_eq!(p.source, v2xw_world::WorldSourceKind::Procedural);
    assert_eq!(p.source_id, MODEL_ID);
    assert_eq!(p.imported_at, "2026-09-18T00:00:00Z");
    assert_eq!(p.content_hash, world.content_hash);
    assert_eq!(p.projection, v2xw_world::Projection::NAME);
    assert!(p.tool_versions.contains_key("v2xw-world"));
    for wanted in ["procedural-grid", "local-tangent-plane", "quantise"] {
        assert!(
            p.transformations.iter().any(|t| t.name == wanted),
            "transformation {wanted} is not recorded (invariant I-W3)"
        );
    }
    assert!(
        p.transformations.iter().any(|t| t.name == "height-default"),
        "a defaulted building height must be recorded (04-models §1.3)"
    );
    assert!(
        world
            .buildings
            .iter()
            .all(|b| b.height_source == v2xw_world::HeightSource::Defaulted)
    );
    assert!(!p.layers.is_empty());
    assert!(
        p.required_attributions().is_empty(),
        "generated data needs none"
    );
    assert_eq!(p.wire_licence(), "Apache-2.0");
}

#[test]
fn lane_positions_survive_a_native_round_trip() {
    let world = rich_grid();
    let lane = LaneId::new(7);
    let pos = LanePos::new(lane, world.lane(lane).length_m * 0.4, 0.75);
    let before = world.to_xyz(&pos);
    let back = serde_native::from_bytes(&serde_native::to_bytes(&world).unwrap()).unwrap();
    assert_eq!(back.to_xyz(&pos), before);
    assert_eq!(back.project(before), world.project(before));
}

// ---------------------------------------------------------------------------
// The two digests (R13)
// ---------------------------------------------------------------------------

/// The payload URL is keyed by the **payload** digest, not by the world's geometry
/// digest, and the two are different numbers.
///
/// vwp-v1 §4.2 says the payload's `content_hash` "MUST equal the URL hash and
/// `Hello.world_hash`", and conformance W1 repeats it. The crate used to document both
/// digests as the one `Hello.world_hash` carries — `hash.rs` claimed the geometry digest,
/// `serde_vwp.rs` the payload digest — which is a contradiction a server implementer would
/// have had to resolve by guessing, and half the guesses produce a `Hello` whose
/// `world_hash` resolves to no payload. The code was always right; this test is what stops
/// the documentation drifting away from it again.
#[test]
fn the_payload_url_is_keyed_by_the_payload_digest() {
    let world = rich_grid();
    let payload = serde_vwp::write(&world).unwrap();

    let geometry_hex = v2xw_core::hash::hex_encode(&world.content_hash);
    let payload_hex = payload.content_hash_hex();
    assert_ne!(
        geometry_hex, payload_hex,
        "the geometry digest and the payload digest hash different things"
    );

    assert_eq!(payload.url_path(), format!("/world/{payload_hex}.vwb"));
    assert!(payload.url_path().contains(&payload_hex));
    assert!(
        !payload.url_path().contains(&geometry_hex),
        "the URL must not be keyed by the geometry digest"
    );

    // A client fetching `GET /world/{hash}.vwb` and verifying it gets the same number,
    // which is the whole content of W1.
    assert_eq!(
        serde_vwp::verify(&payload.bytes).unwrap(),
        payload.content_hash
    );
    // And §4.6: the JSON form carries the *binary* form's digest.
    let json = serde_vwp::to_json(&world).unwrap();
    assert_eq!(json["content_hash"], serde_json::json!(payload_hex));
    // The geometry digest is the one the provenance and the run manifest record.
    assert_eq!(world.provenance.content_hash, world.content_hash);
}

// ---------------------------------------------------------------------------
// D9, continued: finiteness (R5) and the provenance parameters (R12)
// ---------------------------------------------------------------------------

/// `quant::is_on_grid` used to answer *true* for a `NaN` or an infinity, so the D9 scan
/// could not see one, and `validate` had no finiteness check outside `Lane::new` and
/// `Building::new`'s footprint loop. A non-finite float therefore reached both writers —
/// and they disagree about it: the binary payload stores the `NaN` that vwp-v1 §0 makes
/// the "absent" sentinel, while the JSON form writes `null`, so the two are not the
/// "direct transcription" of each other that §4.6 requires.
#[test]
fn a_non_finite_float_is_off_the_grid_and_rejected_by_validate() {
    assert!(!is_on_grid(f64::NAN, Q_POSITION_M));
    assert!(!is_on_grid(f64::INFINITY, Q_POSITION_M));
    assert!(!is_on_grid(f64::NEG_INFINITY, Q_HEIGHT_M));

    // A building height, which no constructor checks.
    let mut parts = rich_grid().to_parts();
    assert!(!parts.buildings.is_empty());
    parts.buildings[0].height_m = f64::NAN;
    let err = World::from_parts(parts).expect_err("a NaN height must not make a world");
    assert!(
        matches!(err, v2xw_world::WorldError::NonFinite { .. }),
        "unexpected error: {err:?}"
    );
    assert!(err.to_string().contains("building.height_m"), "{err}");

    // An infinity anywhere else in the scan, in a column that is neither a coordinate nor
    // a height: a site's antenna gain, in dB.
    let mut parts = rich_grid().to_parts();
    assert!(!parts.sites.is_empty());
    parts.sites[0].antenna_gain_dbi = f64::INFINITY;
    let err = World::from_parts(parts).expect_err("an infinite gain must not make a world");
    assert!(
        matches!(err, v2xw_world::WorldError::NonFinite { .. }),
        "unexpected error: {err:?}"
    );

    // 1e306 is finite but so large that the quantiser cannot scale it; it must survive
    // unchanged rather than becoming an infinity, which is what this crate's private copy
    // of the quantiser used to do (R6).
    assert_eq!(quantise(1e306, Q_POSITION_M), 1e306);
    assert!(is_on_grid(1e306, Q_POSITION_M));
}

/// D9 covers a transformation's float parameters: they are `f64`s serialised verbatim
/// into every artefact this crate writes, so an off-grid one is a raw IEEE-754 double in
/// an exported, digested artefact. The scan used to skip them entirely.
#[test]
fn d9_scan_visits_the_provenance_transformation_parameters() {
    let world = rich_grid();
    let mut paths = Vec::new();
    world.scan_exported_floats(&mut |path, value, quantum| {
        if path.starts_with("provenance.transformations.") {
            assert!(is_on_grid(value, quantum), "{path} = {value}");
            paths.push(path.to_string());
        }
    });
    assert!(
        paths.len() >= 4,
        "the generator records several float parameters; the scan saw {paths:#?}"
    );
    assert!(
        paths
            .iter()
            .any(|p| p.ends_with("procedural-grid.block_x_m")),
        "{paths:#?}"
    );

    // And a parameter written straight into the map, bypassing `Transformation::with`'s
    // quantiser, is caught: `validate` refuses the world instead of writing the raw double
    // into world.json, world.v2xw and the .vwb provenance blob. This is also why the
    // blanket JSON sweep has no off-grid world to be run against — one cannot be built.
    let mut parts = rich_grid().to_parts();
    let mut rogue = v2xw_world::Transformation::new("rogue");
    rogue.params.insert(
        "block_x_m".to_string(),
        serde_json::json!(120.000_000_123_456_7),
    );
    parts.provenance.transformations.push(rogue);
    let err = World::from_parts(parts).expect_err("an off-grid parameter must be refused");
    assert!(
        err.to_string().contains("D9") && err.to_string().contains("block_x_m"),
        "unexpected error: {err}"
    );
}

// ---------------------------------------------------------------------------
// The junction-shape invariant (R11)
// ---------------------------------------------------------------------------

/// A junction's position is inside its own polygon, after quantisation, in every world —
/// and `validate` says so, including for a world read back from an artefact, which never
/// goes through the quantiser.
#[test]
fn every_junction_position_is_inside_its_shape() {
    for world in [small_grid(), rich_grid()] {
        for j in world.roads.junctions() {
            assert!(
                j.position_is_in_shape(),
                "junction {} at {:?} is outside its shape {:?}",
                j.id,
                j.position,
                j.shape
            );
        }
    }

    // Move one junction out of its polygon and `validate` must refuse the world.
    let world = rich_grid();
    let parts = world.to_parts();
    let mut junctions = parts.roads.junctions().to_vec();
    let moved = junctions
        .iter()
        .position(|j| !j.shape.is_empty())
        .expect("the generator gives junctions a shape");
    junctions[moved].position = junctions[moved].position + Vec3::new_2d(1000.0, 1000.0);
    let roads = v2xw_world::RoadNetwork::new(
        parts.roads.lanes().to_vec(),
        parts.roads.edges().to_vec(),
        junctions,
        parts.roads.connections().to_vec(),
        parts.roads.crossings().to_vec(),
    )
    .unwrap();
    let broken = v2xw_world::WorldParts { roads, ..parts };
    let err = World::from_parts(broken).expect_err("a junction outside its shape is invalid");
    assert!(
        err.to_string().contains("junction-shape"),
        "unexpected error: {err}"
    );
}

// ---------------------------------------------------------------------------
// The f32 precision warning (R14)
// ---------------------------------------------------------------------------

/// Every narrowed column warns, not just lane centreline `x` and `y`.
///
/// The payload stores geometry as `f32`, which holds a millimetre grid only out to
/// `F32_MM_GRID_LIMIT_M` = 16 384 m from the origin. A world bigger than that loses
/// millimetres in *every* narrowed column — building and land-use rings, junction
/// positions, signal heads, sites, crossings — and used to report only the two columns
/// somebody happened to instrument.
#[test]
fn precision_warnings_cover_every_narrowed_column() {
    let mut params = GridParams::legacy().with_size(3, 3).with_block_m(9_000.0);
    params.block_buildings = true;
    params.rsu_at_junctions = true;
    params.crossings = true;
    params.signalised = true;
    let world = grid(&params, &opts()).expect("an 18 km grid is still a grid");
    assert!(
        world.bbox.max.x > v2xw_world::quant::F32_MM_GRID_LIMIT_M,
        "the fixture must reach past the f32 limit: {:?}",
        world.bbox
    );

    let payload = serde_vwp::write(&world).unwrap();
    let warnings = &payload.precision_warnings;
    assert!(!warnings.is_empty(), "a 18 km world must warn");
    for section in ["lane", "building", "junction", "signal", "site", "crossing"] {
        assert!(
            warnings.iter().any(|w| w.starts_with(section)),
            "no precision warning for {section}: {warnings:#?}"
        );
    }
    assert!(
        warnings.iter().all(|w| w.contains("16384")),
        "{warnings:#?}"
    );
    // One per column, so the list stays readable and cannot be swamped by whichever
    // section is written first.
    assert!(
        warnings.len() <= serde_vwp::MAX_PRECISION_WARNINGS,
        "{warnings:#?}"
    );

    // The payload is still valid and still deterministic — a warning is not an error.
    assert_eq!(serde_vwp::write(&world).unwrap().bytes, payload.bytes);
    assert_eq!(
        serde_vwp::verify(&payload.bytes).unwrap(),
        payload.content_hash
    );
    // And a world inside the limit says nothing.
    assert!(
        serde_vwp::write(&rich_grid())
            .unwrap()
            .precision_warnings
            .is_empty()
    );
}
