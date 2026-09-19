//! The world **geometry digest** (invariant I-W2).
//!
//! [`content_hash`] is a SHA-256 over all geometry, in quantised integers, independent of
//! any wire format. It **identifies the world**: it is what the run manifest records, what
//! [`crate::model::WorldProvenance::content_hash`] carries, and what
//! [`crate::model::World::from_parts`] checks a deserialised world against.
//!
//! # It is not the payload digest, and not `Hello.world_hash`
//!
//! The crate computes two digests and they are necessarily different numbers, because
//! they hash different things. Naming them apart is the point of this section:
//!
//! | Name | Function | What it hashes | Where it is used |
//! |---|---|---|---|
//! | **geometry digest** | [`content_hash`] | the model, as grid integers | the run manifest, the provenance record, invariant I-W2 |
//! | **payload digest** | [`crate::serde_vwp::WorldPayload::content_hash`] | the `vwp-world/1` body bytes | `GET /world/{hash}.vwb`, and `Hello.world_hash` |
//!
//! **The protocol's `Hello.world_hash` is the payload digest.** docs/protocol/vwp-v1.md
//! §4.2 says of the payload's own `content_hash` field: "SHA-256 of the body; MUST equal
//! the URL hash and `Hello.world_hash`", conformance item W1 (§10.5) repeats it —
//! "`GET /world/{hash}.vwb` returns a body whose SHA-256 equals `{hash}` and
//! `Hello.world_hash`" — and §4's caching rule only works that way round: "a client that
//! already holds the world for `Hello.world_hash` skips the fetch entirely" is a statement
//! about a URL key. §3.1.1's *note* column glosses the field as "`World.content_hash`
//! (03-interfaces §2)", which is this module's digest; that gloss is the stale spelling,
//! it contradicts a `MUST`, and a server that wires it up produces a `Hello` whose
//! `world_hash` resolves to no payload. It is flagged as a defect in vwp-v1 §4.2's
//! amendment note.
//!
//! Two worlds with the same geometry digest are interchangeable, so it has to be stable
//! across platforms, across runs and across anything that is not geometry.
//!
//! # What makes it stable
//!
//! 1. **Every float is hashed as an integer.** A coordinate is absorbed as
//!    [`crate::quant::grid_index`] — the integer multiple of its quantum — never as its
//!    IEEE-754 bits. The representation error of `k · 0.001` therefore cannot reach the
//!    digest, and neither can a platform's rounding of a transcendental: that is the
//!    whole lesson of the legacy digest forensics (D9).
//! 2. **Every section is tagged and counted.** Each section begins with a one-byte tag
//!    and a `u32` count, so no rearrangement of two sections can produce the same byte
//!    stream, and a string is always length-prefixed.
//! 3. **The order is fixed and documented** — see [`content_hash`].
//! 4. **Names are hashed as text, not as symbol ids.** Two importers that intern the same
//!    names in a different order build different [`crate::model::SymbolTable`]s and the
//!    same hash, which is the correct behaviour: the interning order is an implementation
//!    detail, the names are the geometry.
//!
//! # What is deliberately excluded
//!
//! The provenance record: the source id, the import date, the tool versions, the
//! transformations and the licences. The same geometry imported twice, on two days, by
//! two builds, must hash the same — otherwise the content-addressed world cache misses
//! every time and the `Hello.world_hash` of a re-run changes for no reason a viewer can
//! see. Invariant I-W2 says the hash covers all *geometry*, and this is that reading. The
//! provenance is carried alongside the hash, in the manifest and in the payload, so
//! nothing is lost.
//!
//! The index options and the lazily built indices are excluded for the same reason: a
//! grid cell size is a performance knob, not geometry.

use sha2::{Digest, Sha256};

use v2xw_core::geom::Vec3;

use crate::model::{
    Building, Connection, Crossing, Junction, LanduseZone, Lane, SignalPlan, Site, Terrain, World,
};
use crate::quant::{Q_DB, Q_DEGREES, Q_HEIGHT_M, Q_POSITION_M, Q_SPEED_MPS, Q_TIME_S, grid_index};

/// The domain separator, so this digest can never collide with another SHA-256 in the
/// engine (a model card's, a parameter set's, the manifest's).
const DOMAIN: &[u8] = b"v2xw-world/content/1";

/// Section tags, in the order [`content_hash`] absorbs them.
mod tag {
    /// Origin, bounding box and world-level scalars.
    pub const HEADER: u8 = 0x01;
    /// Lanes.
    pub const LANES: u8 = 0x02;
    /// Edges.
    pub const EDGES: u8 = 0x03;
    /// Junctions.
    pub const JUNCTIONS: u8 = 0x04;
    /// Connections.
    pub const CONNECTIONS: u8 = 0x05;
    /// Crossings.
    pub const CROSSINGS: u8 = 0x06;
    /// Buildings.
    pub const BUILDINGS: u8 = 0x07;
    /// Terrain.
    pub const TERRAIN: u8 = 0x08;
    /// Signal plans.
    pub const SIGNALS: u8 = 0x09;
    /// Sites.
    pub const SITES: u8 = 0x0A;
    /// Land-use zones.
    pub const LANDUSE: u8 = 0x0B;
    /// End marker, so a truncated stream cannot hash like a complete one.
    pub const END: u8 = 0xFF;
}

/// Absorbs typed values into a SHA-256 in a self-delimiting way.
///
/// Public because the importer and the recorder hash their own artefacts the same way; a
/// consistent absorbing rule across the engine is worth more than a private type.
#[derive(Debug, Clone)]
pub struct GeometryHasher {
    digest: Sha256,
}

impl GeometryHasher {
    /// A hasher primed with this crate's domain separator.
    pub fn new() -> Self {
        let mut digest = Sha256::new();
        digest.update(DOMAIN);
        Self { digest }
    }

    /// Absorbs a one-byte tag.
    pub fn tag(&mut self, t: u8) -> &mut Self {
        self.digest.update([t]);
        self
    }

    /// Absorbs an unsigned 32-bit value, little-endian.
    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.digest.update(v.to_le_bytes());
        self
    }

    /// Absorbs an unsigned 64-bit value, little-endian.
    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.digest.update(v.to_le_bytes());
        self
    }

    /// Absorbs a signed 64-bit value, little-endian.
    pub fn i64(&mut self, v: i64) -> &mut Self {
        self.digest.update(v.to_le_bytes());
        self
    }

    /// Absorbs a count, as a `u32`.
    pub fn count(&mut self, n: usize) -> &mut Self {
        self.u32(u32::try_from(n).unwrap_or(u32::MAX))
    }

    /// Absorbs a float **as its grid integer** ([`crate::quant::grid_index`]).
    pub fn quantised(&mut self, value: f64, quantum: f64) -> &mut Self {
        self.i64(grid_index(value, quantum))
    }

    /// Absorbs a point: `x` and `y` on the position grid, `z` on the height grid.
    pub fn point(&mut self, p: Vec3) -> &mut Self {
        self.quantised(p.x, Q_POSITION_M)
            .quantised(p.y, Q_POSITION_M)
            .quantised(p.z, Q_HEIGHT_M)
    }

    /// Absorbs a polyline or ring: its length, then its points.
    pub fn points(&mut self, points: &[Vec3]) -> &mut Self {
        self.count(points.len());
        for p in points {
            self.point(*p);
        }
        self
    }

    /// Absorbs a string: its byte length, then its bytes.
    pub fn text(&mut self, s: &str) -> &mut Self {
        self.count(s.len());
        self.digest.update(s.as_bytes());
        self
    }

    /// Absorbs a flag.
    pub fn bool(&mut self, b: bool) -> &mut Self {
        self.digest.update([u8::from(b)]);
        self
    }

    /// Absorbs an optional id as `Some → (1, value)`, `None → (0, 0)`.
    pub fn optional_id(&mut self, id: Option<u32>) -> &mut Self {
        match id {
            Some(v) => self.bool(true).u32(v),
            None => self.bool(false).u32(0),
        }
    }

    /// Finishes and returns the digest.
    pub fn finish(self) -> [u8; 32] {
        self.digest.finalize().into()
    }
}

impl Default for GeometryHasher {
    fn default() -> Self {
        Self::new()
    }
}

/// The **geometry digest** of a world (invariant I-W2): the number that identifies the
/// world, in the run manifest and in its provenance record.
///
/// Not the number that addresses the payload — see this module's header, and
/// [`crate::serde_vwp::WorldPayload::content_hash`], for the other digest and for which
/// one `Hello.world_hash` carries.
///
/// # The order
///
/// Sections are absorbed in this order, each preceded by its tag and its element count:
///
/// 1. **header** — origin latitude, longitude and altitude; the bounding box's two
///    corners; the default environment class;
/// 2. **lanes**, by `LaneId`: id, edge, junction, index, kind, width, speed limit,
///    allowed classes, centreline, length;
/// 3. **edges**, by `EdgeId`: id, endpoints, lane list, name text, road class;
/// 4. **junctions**, by `JunctionId`: id, position, shape, incoming, outgoing and
///    internal lane lists, control (and its plan), conflict matrix, name text;
/// 5. **connections**, in the network's stored order (sorted by
///    `(from_lane, to_lane, via)`);
/// 6. **crossings**, by `CrossingId`;
/// 7. **buildings**, by `BuildingId`: footprint, holes, heights, levels, material,
///    height source, level of detail, name text;
/// 8. **terrain**: present flag, grid origin, spacing, size, interpolation, heights;
/// 9. **signal plans**, by `SignalId`: junction, cycle, offset, controlled movements,
///    phases and their states, heads;
/// 10. **sites**, by `SiteId`; 11. **land-use zones**, by `ZoneId`; then an end marker.
///
/// Every float is absorbed as its grid integer, so the digest is a function of the
/// *quantised* world and nothing else.
pub fn content_hash(world: &World) -> [u8; 32] {
    let mut h = GeometryHasher::new();

    h.tag(tag::HEADER);
    h.quantised(world.origin.lat_deg, Q_DEGREES)
        .quantised(world.origin.lon_deg, Q_DEGREES)
        .quantised(world.origin.alt_m, Q_HEIGHT_M)
        .point(world.bbox.min)
        .point(world.bbox.max)
        .text(world.default_env.label());

    h.tag(tag::LANES).count(world.roads.lanes().len());
    for lane in world.roads.lanes() {
        hash_lane(&mut h, lane);
    }

    h.tag(tag::EDGES).count(world.roads.edges().len());
    for edge in world.roads.edges() {
        h.u32(edge.id.index())
            .u32(edge.from.index())
            .u32(edge.to.index())
            .count(edge.lanes.len());
        for l in &edge.lanes {
            h.u32(l.index());
        }
        h.text(world.symbols.resolve_optional(edge.name));
        h.text(edge.road_class.label());
    }

    h.tag(tag::JUNCTIONS).count(world.roads.junctions().len());
    for j in world.roads.junctions() {
        hash_junction(&mut h, world, j);
    }

    h.tag(tag::CONNECTIONS)
        .count(world.roads.connections().len());
    for c in world.roads.connections() {
        hash_connection(&mut h, c);
    }

    h.tag(tag::CROSSINGS).count(world.roads.crossings().len());
    for c in world.roads.crossings() {
        hash_crossing(&mut h, c);
    }

    h.tag(tag::BUILDINGS).count(world.buildings.len());
    for b in &world.buildings {
        hash_building(&mut h, world, b);
    }

    h.tag(tag::TERRAIN);
    match &world.terrain {
        None => {
            h.bool(false);
        }
        Some(t) => {
            h.bool(true);
            hash_terrain(&mut h, t);
        }
    }

    h.tag(tag::SIGNALS).count(world.signals.len());
    for plan in &world.signals {
        hash_signal_plan(&mut h, world, plan);
    }

    h.tag(tag::SITES).count(world.sites.len());
    for s in &world.sites {
        hash_site(&mut h, world, s);
    }

    h.tag(tag::LANDUSE).count(world.landuse.len());
    for z in &world.landuse {
        hash_zone(&mut h, world, z);
    }

    h.tag(tag::END);
    h.finish()
}

fn hash_lane(h: &mut GeometryHasher, lane: &Lane) {
    h.u32(lane.id.index())
        .u32(lane.edge.index())
        .optional_id(lane.junction.map(|j| j.index()))
        .u32(u32::from(lane.index))
        .u32(u32::from(lane.kind.wire_code()))
        .quantised(lane.width_m, Q_POSITION_M)
        .quantised(lane.speed_limit_mps, Q_SPEED_MPS)
        .u32(u32::from(lane.allowed.bits()))
        .points(&lane.centreline)
        .quantised(lane.length_m, Q_POSITION_M);
}

fn hash_junction(h: &mut GeometryHasher, world: &World, j: &Junction) {
    h.u32(j.id.index()).point(j.position).points(&j.shape);
    for list in [&j.incoming, &j.outgoing, &j.internal] {
        h.count(list.len());
        for l in list {
            h.u32(l.index());
        }
    }
    h.u32(u32::from(j.control.wire_code()))
        .optional_id(j.control.plan().map(|p| p.index()));
    let (foes, response) = j.conflicts.raw();
    h.count(j.conflicts.len()).count(foes.len());
    for w in foes.iter().chain(response.iter()) {
        h.u64(*w);
    }
    h.text(world.symbols.resolve_optional(j.name));
}

fn hash_connection(h: &mut GeometryHasher, c: &Connection) {
    h.u32(c.from_lane.index())
        .u32(c.to_lane.index())
        .optional_id(c.via.map(|v| v.index()))
        .text(c.direction.label())
        .bool(c.permitted);
}

fn hash_crossing(h: &mut GeometryHasher, c: &Crossing) {
    h.u32(c.id.index())
        .u32(c.junction.index())
        .point(c.from)
        .point(c.to)
        .quantised(c.width_m, Q_POSITION_M)
        .bool(c.priority);
}

fn hash_building(h: &mut GeometryHasher, world: &World, b: &Building) {
    h.u32(b.id.index())
        .points(&b.footprint)
        .count(b.holes.len());
    for hole in &b.holes {
        h.points(hole);
    }
    h.quantised(b.height_m, Q_HEIGHT_M)
        .quantised(b.min_height_m, Q_HEIGHT_M)
        .quantised(b.base_z_m, Q_HEIGHT_M)
        .optional_id(b.levels.map(u32::from))
        .u32(u32::from(b.material.wire_code()))
        .text(b.height_source.label())
        .u32(u32::from(b.lod.wire_code()))
        .text(world.symbols.resolve_optional(b.name));
}

fn hash_terrain(h: &mut GeometryHasher, t: &Terrain) {
    h.quantised(t.origin_x_m, Q_POSITION_M)
        .quantised(t.origin_y_m, Q_POSITION_M)
        .quantised(t.cell_x_m, Q_POSITION_M)
        .quantised(t.cell_y_m, Q_POSITION_M)
        .u32(t.nx)
        .u32(t.ny)
        .text(t.interpolation.label())
        .count(t.heights_m.len());
    for height in &t.heights_m {
        h.quantised(*height, Q_HEIGHT_M);
    }
}

fn hash_signal_plan(h: &mut GeometryHasher, world: &World, plan: &SignalPlan) {
    h.u32(plan.id.index())
        .u32(plan.junction.index())
        .quantised(plan.cycle_s, Q_TIME_S)
        .quantised(plan.offset_s, Q_TIME_S)
        .count(plan.controlled.len());
    for l in &plan.controlled {
        h.u32(l.index());
    }
    h.count(plan.phases.len());
    for phase in &plan.phases {
        h.quantised(phase.duration_s, Q_TIME_S)
            .count(phase.states.len());
        for state in &phase.states {
            h.u32(u32::from(state.sumo_letter() as u8));
        }
        h.text(world.symbols.resolve_optional(phase.name));
    }
    h.count(plan.heads.len());
    for head in &plan.heads {
        h.u32(head.lane.index())
            .point(head.position)
            .u32(u32::from(head.kind.wire_code()))
            .u32(u32::from(head.group));
    }
}

fn hash_site(h: &mut GeometryHasher, world: &World, s: &Site) {
    h.u32(s.id.index())
        .optional_id(s.node.map(|n| n.index()))
        .point(s.position)
        .quantised(s.antenna_height_m, Q_HEIGHT_M)
        .quantised(s.antenna_gain_dbi, Q_DB)
        .u32(u32::from(s.kind.wire_code()))
        .text(world.symbols.resolve_optional(s.name));
}

fn hash_zone(h: &mut GeometryHasher, world: &World, z: &LanduseZone) {
    h.u32(z.id.index())
        .points(&z.ring)
        .u32(u32::from(z.class.wire_code()))
        .text(z.env.label())
        .text(world.symbols.resolve_optional(z.name));
}

/// The content hash as lower-case hex — the form the `GET /world/{hash}.vwb` URL and the
/// run manifest use.
pub fn content_hash_hex(world: &World) -> String {
    v2xw_core::hash::hex_encode(&world.content_hash)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{GeoOrigin, SymbolTable};
    use crate::procedural::{GridParams, grid};

    fn world() -> World {
        grid(
            &GridParams::legacy().with_size(3, 3),
            &crate::ImportOptions::default().imported_at("2026-09-18T00:00:00Z"),
        )
        .unwrap()
    }

    #[test]
    fn hasher_is_self_delimiting() {
        // Two different field splits that concatenate to the same bytes must not collide,
        // which is what the length prefixes buy.
        let mut a = GeometryHasher::new();
        a.text("ab").text("c");
        let mut b = GeometryHasher::new();
        b.text("a").text("bc");
        assert_ne!(a.clone().finish(), b.finish());

        // And the domain separator makes this hash unlike a bare SHA-256.
        assert_ne!(GeometryHasher::new().finish(), v2xw_core::hash::sha256(b""));
    }

    #[test]
    fn floats_are_hashed_as_grid_integers() {
        // Two values a hair either side of the same millimetre hash identically, because
        // the digest sees the integer, not the double. This is the property that makes
        // the hash survive a different maths library (D9).
        let mut a = GeometryHasher::new();
        a.quantised(12.346_000_000_000_001, 1e-3);
        let mut b = GeometryHasher::new();
        b.quantised(12.345_999_999_999_999, 1e-3);
        assert_eq!(a.clone().finish(), b.finish());

        let mut c = GeometryHasher::new();
        c.quantised(12.347, 1e-3);
        assert_ne!(a.finish(), c.finish());
    }

    #[test]
    fn content_hash_is_a_function_of_the_geometry_only() {
        let w = world();
        assert_eq!(content_hash(&w), w.content_hash);
        assert_eq!(
            content_hash_hex(&w),
            v2xw_core::hash::hex_encode(&w.content_hash)
        );
        assert_eq!(content_hash_hex(&w).len(), 64);

        // The provenance is not hashed: changing every field of it changes nothing.
        let mut other = w.clone();
        other.provenance.imported_at = "1970-01-01T00:00:00Z".to_string();
        other.provenance.source_id = "something else".to_string();
        other.provenance.notes.push("a note".to_string());
        other
            .provenance
            .tool_versions
            .insert("netconvert".to_string(), "1.19.0".to_string());
        assert_eq!(content_hash(&other), w.content_hash);

        // Nor are the index options, which are a performance knob.
        let mut other = w.clone();
        other.index_options.lane_grid_cell_m = 3.0;
        assert_eq!(content_hash(&other), w.content_hash);

        // But every geometry section is.
        let mut moved = w.clone();
        moved.bbox.max.x += 0.001;
        assert_ne!(content_hash(&moved), w.content_hash);

        let mut env = w.clone();
        env.default_env = crate::model::EnvClass::Rural;
        assert_ne!(content_hash(&env), w.content_hash);

        let mut origin = w.clone();
        origin.origin = GeoOrigin::new(52.5163, 13.3777, 34.0);
        assert_ne!(content_hash(&origin), w.content_hash);
    }

    #[test]
    fn content_hash_ignores_the_interning_order_but_not_the_names() {
        let w = world();
        // Re-intern every name in the opposite order: the same strings, different symbol
        // ids. The hash absorbs resolved text, so it must not move — and `from_parts`
        // proves it, because it refuses a world whose stored hash and geometry disagree.
        let mut table = SymbolTable::new();
        let mut remap = vec![0u32; w.symbols.len()];
        for (i, text) in w.symbols.strings().iter().enumerate().rev() {
            remap[i] = table.intern(text).index();
        }
        let rename = |id: Option<crate::model::SymbolId>| {
            id.map(|s| crate::model::SymbolId::new(remap[s.as_usize()]))
        };

        let mut parts = w.to_parts();
        let mut edges = parts.roads.edges().to_vec();
        for e in &mut edges {
            e.name = rename(e.name);
        }
        let mut junctions = parts.roads.junctions().to_vec();
        for j in &mut junctions {
            j.name = rename(j.name);
        }
        for b in &mut parts.buildings {
            b.name = rename(b.name);
        }
        for s in &mut parts.sites {
            s.name = rename(s.name);
        }
        for z in &mut parts.landuse {
            z.name = rename(z.name);
        }
        parts.roads = crate::model::RoadNetwork::new(
            parts.roads.lanes().to_vec(),
            edges,
            junctions,
            parts.roads.connections().to_vec(),
            parts.roads.crossings().to_vec(),
        )
        .unwrap();
        parts.symbols = table;
        assert_ne!(parts.symbols, w.symbols, "the test must actually renumber");

        let shuffled = World::from_parts(parts)
            .expect("the same names in a different interning order hash the same");
        assert_eq!(shuffled.content_hash, w.content_hash);

        // Changing a name, rather than its id, does move the hash — so the world's own
        // stored hash no longer matches and `from_parts` refuses it.
        let mut parts = w.to_parts();
        let mut edges = parts.roads.edges().to_vec();
        let renamed = parts.symbols.intern("Renamed Street");
        edges[0].name = Some(renamed);
        parts.roads = crate::model::RoadNetwork::new(
            parts.roads.lanes().to_vec(),
            edges,
            parts.roads.junctions().to_vec(),
            parts.roads.connections().to_vec(),
            parts.roads.crossings().to_vec(),
        )
        .unwrap();
        assert!(
            World::from_parts(parts).is_err(),
            "a renamed street is a different world"
        );
    }
}
