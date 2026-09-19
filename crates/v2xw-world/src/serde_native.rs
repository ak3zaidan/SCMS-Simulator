//! The engine's own world format: a versioned binary container, plus a JSON form for
//! debugging.
//!
//! This is *not* the `vwp-world/1` payload of [`crate::serde_vwp`]. That one is the UI's:
//! `f32` columns, outer rings only, no lane graph, tuned for a renderer that fetches it
//! once by content hash. This one is the engine's: every field, `f64` bits preserved,
//! exact round trip, so a world can be written to a cache and read back as the same
//! object — same content hash, same connections, same conflict matrices.
//!
//! # Container layout
//!
//! All integers are little-endian.
//!
//! ```text
//! magic        8 bytes   "V2XWWRLD"
//! version      u16       1
//! reserved     u16       0
//! sections     u32       number of table entries
//! table        24·n      { kind u16, flags u16, reserved u32, offset u64, length u64 }
//! payloads               each section's bytes, in table order, 8-byte aligned
//! ```
//!
//! A reader **skips a section whose kind it does not know**, which is what makes the
//! format additively extensible: a later version that adds, say, a lane-marking section
//! is still readable by this one. Removing or changing a section is a version bump.
//!
//! # Exactness
//!
//! Floats are written as their IEEE-754 bits, so the round trip is exact rather than
//! merely close. There is no conflict with D9: every float in a `World` is already on its
//! quantisation grid ([`crate::quant`]), so writing its bits writes an on-grid value, and
//! the `native_round_trip_is_exact` test checks the whole world compares equal.

use std::collections::BTreeMap;

use v2xw_core::geom::{Bbox, Vec3};
use v2xw_core::ids::{BuildingId, EdgeId, JunctionId, LaneId, NodeId, SignalId};

use crate::error::{Result, WorldError};
use crate::model::{
    Building, ClassMask, ConflictMatrix, Connection, Crossing, CrossingId, Edge, EnvClass,
    GeoOrigin, HeightSource, Interpolation, Junction, JunctionControl, LanduseClass, LanduseZone,
    Lane, LaneKind, LodHint, MaterialClass, RoadClass, RoadNetwork, SignalHead, SignalHeadKind,
    SignalPhase, SignalPlan, SignalState, Site, SiteId, SiteKind, SymbolId, SymbolTable, Terrain,
    TurnDirection, World, WorldParts, WorldProvenance, ZoneId,
};

/// The container's magic bytes.
pub const MAGIC: [u8; 8] = *b"V2XWWRLD";

/// The container format version this module writes.
pub const FORMAT_VERSION: u16 = 1;

/// Bytes of the fixed container header, before the section table.
pub const HEADER_BYTES: usize = 16;

/// Bytes of one section-table entry.
pub const TABLE_ENTRY_BYTES: usize = 24;

/// Section kinds. Unknown kinds are skipped by a reader.
pub mod section {
    /// Origin, bounding box, default environment, index options, content hash.
    pub const HEADER: u16 = 1;
    /// The symbol table.
    pub const SYMBOLS: u16 = 2;
    /// Lanes.
    pub const LANES: u16 = 3;
    /// Edges.
    pub const EDGES: u16 = 4;
    /// Junctions.
    pub const JUNCTIONS: u16 = 5;
    /// Connections.
    pub const CONNECTIONS: u16 = 6;
    /// Crossings.
    pub const CROSSINGS: u16 = 7;
    /// Buildings.
    pub const BUILDINGS: u16 = 8;
    /// The terrain grid.
    pub const TERRAIN: u16 = 9;
    /// Signal plans.
    pub const SIGNALS: u16 = 10;
    /// Sites.
    pub const SITES: u16 = 11;
    /// Land-use zones.
    pub const LANDUSE: u16 = 12;
    /// The provenance record, as UTF-8 JSON.
    pub const PROVENANCE: u16 = 13;
}

// ---------------------------------------------------------------------------
// Primitive writer and reader
// ---------------------------------------------------------------------------

/// Appends little-endian primitives to a byte buffer.
#[derive(Debug, Default)]
struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    fn new() -> Self {
        Self { buf: Vec::new() }
    }

    fn u8(&mut self, v: u8) -> &mut Self {
        self.buf.push(v);
        self
    }

    fn u16(&mut self, v: u16) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    fn u32(&mut self, v: u32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    fn u64(&mut self, v: u64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    fn f64(&mut self, v: f64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_bits().to_le_bytes());
        self
    }

    fn bool(&mut self, v: bool) -> &mut Self {
        self.u8(u8::from(v))
    }

    fn count(&mut self, n: usize) -> &mut Self {
        self.u32(n as u32)
    }

    fn text(&mut self, s: &str) -> &mut Self {
        self.count(s.len());
        self.buf.extend_from_slice(s.as_bytes());
        self
    }

    fn point(&mut self, p: Vec3) -> &mut Self {
        self.f64(p.x).f64(p.y).f64(p.z)
    }

    fn points(&mut self, ps: &[Vec3]) -> &mut Self {
        self.count(ps.len());
        for p in ps {
            self.point(*p);
        }
        self
    }

    fn ids(&mut self, ids: impl IntoIterator<Item = u32>, len: usize) -> &mut Self {
        self.count(len);
        for id in ids {
            self.u32(id);
        }
        self
    }

    fn optional_u32(&mut self, v: Option<u32>) -> &mut Self {
        match v {
            Some(x) => self.bool(true).u32(x),
            None => self.bool(false).u32(0),
        }
    }
}

/// Reads little-endian primitives out of a byte slice, refusing to run off the end.
#[derive(Debug)]
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
    /// Where this slice starts in the whole file, so an error points at the real offset.
    base: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8], base: usize) -> Self {
        Self { bytes, at: 0, base }
    }

    fn fail<T>(&self, problem: impl Into<String>) -> Result<T> {
        Err(WorldError::Malformed {
            offset: self.base + self.at,
            problem: problem.into(),
        })
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.at + n > self.bytes.len() {
            return self.fail(format!(
                "wanted {n} bytes, only {} left",
                self.bytes.len() - self.at
            ));
        }
        let out = &self.bytes[self.at..self.at + n];
        self.at += n;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_bits(self.u64()?))
    }

    fn bool(&mut self) -> Result<bool> {
        Ok(self.u8()? != 0)
    }

    fn count(&mut self) -> Result<usize> {
        let n = self.u32()? as usize;
        // A count is always followed by at least one byte per element, so a count larger
        // than the bytes that remain is corruption, not a huge world.
        if n > self.bytes.len() - self.at + 1 {
            return self.fail(format!(
                "count {n} exceeds the {} bytes left",
                self.bytes.len() - self.at
            ));
        }
        Ok(n)
    }

    fn text(&mut self) -> Result<String> {
        let n = self.count()?;
        let b = self.take(n)?;
        match core::str::from_utf8(b) {
            Ok(s) => Ok(s.to_string()),
            Err(e) => self.fail(format!("invalid UTF-8: {e}")),
        }
    }

    fn point(&mut self) -> Result<Vec3> {
        Ok(Vec3::new(self.f64()?, self.f64()?, self.f64()?))
    }

    fn points(&mut self) -> Result<Vec<Vec3>> {
        let n = self.count()?;
        let mut out = Vec::with_capacity(n.min(1 << 16));
        for _ in 0..n {
            out.push(self.point()?);
        }
        Ok(out)
    }

    fn ids<T>(&mut self, wrap: impl Fn(u32) -> T) -> Result<Vec<T>> {
        let n = self.count()?;
        let mut out = Vec::with_capacity(n.min(1 << 16));
        for _ in 0..n {
            out.push(wrap(self.u32()?));
        }
        Ok(out)
    }

    fn optional_u32(&mut self) -> Result<Option<u32>> {
        let present = self.bool()?;
        let value = self.u32()?;
        Ok(if present { Some(value) } else { None })
    }
}

// ---------------------------------------------------------------------------
// Enum codes
// ---------------------------------------------------------------------------

/// The native code of a [`RoadClass`]. Spelled out so that reordering the enum is a
/// deliberate format change.
const fn road_class_code(c: RoadClass) -> u8 {
    match c {
        RoadClass::Motorway => 0,
        RoadClass::Trunk => 1,
        RoadClass::Primary => 2,
        RoadClass::Secondary => 3,
        RoadClass::Tertiary => 4,
        RoadClass::Residential => 5,
        RoadClass::Living => 6,
        RoadClass::Service => 7,
        RoadClass::Link => 8,
        RoadClass::Footway => 9,
        RoadClass::Cycleway => 10,
        RoadClass::Path => 11,
        RoadClass::Internal => 12,
        RoadClass::Unclassified => 13,
    }
}

const fn road_class_from(code: u8) -> Option<RoadClass> {
    match code {
        0 => Some(RoadClass::Motorway),
        1 => Some(RoadClass::Trunk),
        2 => Some(RoadClass::Primary),
        3 => Some(RoadClass::Secondary),
        4 => Some(RoadClass::Tertiary),
        5 => Some(RoadClass::Residential),
        6 => Some(RoadClass::Living),
        7 => Some(RoadClass::Service),
        8 => Some(RoadClass::Link),
        9 => Some(RoadClass::Footway),
        10 => Some(RoadClass::Cycleway),
        11 => Some(RoadClass::Path),
        12 => Some(RoadClass::Internal),
        13 => Some(RoadClass::Unclassified),
        _ => None,
    }
}

const fn turn_code(t: TurnDirection) -> u8 {
    match t {
        TurnDirection::Straight => 0,
        TurnDirection::Left => 1,
        TurnDirection::Right => 2,
        TurnDirection::SlightLeft => 3,
        TurnDirection::SlightRight => 4,
        TurnDirection::UTurn => 5,
    }
}

const fn turn_from(code: u8) -> Option<TurnDirection> {
    match code {
        0 => Some(TurnDirection::Straight),
        1 => Some(TurnDirection::Left),
        2 => Some(TurnDirection::Right),
        3 => Some(TurnDirection::SlightLeft),
        4 => Some(TurnDirection::SlightRight),
        5 => Some(TurnDirection::UTurn),
        _ => None,
    }
}

const fn signal_state_code(s: SignalState) -> u8 {
    match s {
        SignalState::Red => 0,
        SignalState::RedAmber => 1,
        SignalState::Amber => 2,
        SignalState::Green => 3,
        SignalState::GreenYield => 4,
        SignalState::FlashingAmber => 5,
        SignalState::Off => 6,
    }
}

const fn signal_state_from(code: u8) -> Option<SignalState> {
    match code {
        0 => Some(SignalState::Red),
        1 => Some(SignalState::RedAmber),
        2 => Some(SignalState::Amber),
        3 => Some(SignalState::Green),
        4 => Some(SignalState::GreenYield),
        5 => Some(SignalState::FlashingAmber),
        6 => Some(SignalState::Off),
        _ => None,
    }
}

const fn env_code(e: EnvClass) -> u8 {
    match e {
        EnvClass::Urban => 0,
        EnvClass::Suburban => 1,
        EnvClass::Highway => 2,
        EnvClass::Rural => 3,
    }
}

const fn env_from(code: u8) -> Option<EnvClass> {
    match code {
        0 => Some(EnvClass::Urban),
        1 => Some(EnvClass::Suburban),
        2 => Some(EnvClass::Highway),
        3 => Some(EnvClass::Rural),
        _ => None,
    }
}

const fn height_source_code(h: HeightSource) -> u8 {
    match h {
        HeightSource::Tagged => 0,
        HeightSource::FromLevels => 1,
        HeightSource::Defaulted => 2,
        HeightSource::FromParts => 3,
    }
}

const fn height_source_from(code: u8) -> Option<HeightSource> {
    match code {
        0 => Some(HeightSource::Tagged),
        1 => Some(HeightSource::FromLevels),
        2 => Some(HeightSource::Defaulted),
        3 => Some(HeightSource::FromParts),
        _ => None,
    }
}

const fn interpolation_code(i: Interpolation) -> u8 {
    match i {
        Interpolation::Bilinear => 0,
        Interpolation::Nearest => 1,
    }
}

const fn interpolation_from(code: u8) -> Option<Interpolation> {
    match code {
        0 => Some(Interpolation::Bilinear),
        1 => Some(Interpolation::Nearest),
        _ => None,
    }
}

const fn signal_head_kind_from(code: u8) -> Option<SignalHeadKind> {
    match code {
        0 => Some(SignalHeadKind::Vehicle),
        1 => Some(SignalHeadKind::Pedestrian),
        2 => Some(SignalHeadKind::Bicycle),
        3 => Some(SignalHeadKind::Transit),
        _ => None,
    }
}

const fn site_kind_from(code: u8) -> Option<SiteKind> {
    match code {
        0 => Some(SiteKind::Rsu),
        1 => Some(SiteKind::Cell),
        2 => Some(SiteKind::Other),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// Serialises a world into the versioned binary container.
///
/// The result is byte-identical for byte-identical worlds: every collection is written in
/// id order, every map in key order, and nothing consults a hash table.
pub fn to_bytes(world: &World) -> Result<Vec<u8>> {
    let mut sections: Vec<(u16, Vec<u8>)> = Vec::new();

    let mut w = Writer::new();
    w.f64(world.origin.lat_deg)
        .f64(world.origin.lon_deg)
        .f64(world.origin.alt_m)
        .point(world.bbox.min)
        .point(world.bbox.max)
        .u8(env_code(world.default_env))
        .f64(world.index_options.lane_grid_cell_m)
        .u64(world.index_options.max_lane_grid_cells as u64);
    w.buf.extend_from_slice(&world.content_hash);
    sections.push((section::HEADER, core::mem::take(&mut w.buf)));

    let mut w = Writer::new();
    w.count(world.symbols.strings().len());
    for s in world.symbols.strings() {
        w.text(s);
    }
    sections.push((section::SYMBOLS, core::mem::take(&mut w.buf)));

    let mut w = Writer::new();
    w.count(world.roads.lanes().len());
    for lane in world.roads.lanes() {
        w.u32(lane.id.index())
            .u32(lane.edge.index())
            .optional_u32(lane.junction.map(|j| j.index()))
            .u8(lane.index)
            .u8(lane.kind.wire_code())
            .f64(lane.width_m)
            .f64(lane.speed_limit_mps)
            .u16(lane.allowed.bits())
            .points(&lane.centreline);
    }
    sections.push((section::LANES, core::mem::take(&mut w.buf)));

    let mut w = Writer::new();
    w.count(world.roads.edges().len());
    for e in world.roads.edges() {
        w.u32(e.id.index())
            .u32(e.from.index())
            .u32(e.to.index())
            .ids(e.lanes.iter().map(|l| l.index()), e.lanes.len())
            .optional_u32(e.name.map(|n| n.index()))
            .u8(road_class_code(e.road_class));
    }
    sections.push((section::EDGES, core::mem::take(&mut w.buf)));

    let mut w = Writer::new();
    w.count(world.roads.junctions().len());
    for j in world.roads.junctions() {
        w.u32(j.id.index()).point(j.position).points(&j.shape);
        for list in [&j.incoming, &j.outgoing, &j.internal] {
            w.ids(list.iter().map(|l| l.index()), list.len());
        }
        w.u8(j.control.wire_code())
            .optional_u32(j.control.plan().map(|p| p.index()));
        let (foes, response) = j.conflicts.raw();
        w.count(j.conflicts.len()).count(foes.len());
        for word in foes.iter().chain(response.iter()) {
            w.u64(*word);
        }
        w.optional_u32(j.name.map(|n| n.index()));
    }
    sections.push((section::JUNCTIONS, core::mem::take(&mut w.buf)));

    let mut w = Writer::new();
    w.count(world.roads.connections().len());
    for c in world.roads.connections() {
        w.u32(c.from_lane.index())
            .u32(c.to_lane.index())
            .optional_u32(c.via.map(|v| v.index()))
            .u8(turn_code(c.direction))
            .bool(c.permitted);
    }
    sections.push((section::CONNECTIONS, core::mem::take(&mut w.buf)));

    let mut w = Writer::new();
    w.count(world.roads.crossings().len());
    for c in world.roads.crossings() {
        w.u32(c.id.index())
            .u32(c.junction.index())
            .point(c.from)
            .point(c.to)
            .f64(c.width_m)
            .bool(c.priority);
    }
    sections.push((section::CROSSINGS, core::mem::take(&mut w.buf)));

    let mut w = Writer::new();
    w.count(world.buildings.len());
    for b in &world.buildings {
        w.u32(b.id.index())
            .points(&b.footprint)
            .count(b.holes.len());
        for hole in &b.holes {
            w.points(hole);
        }
        w.f64(b.height_m)
            .f64(b.min_height_m)
            .f64(b.base_z_m)
            .optional_u32(b.levels.map(u32::from))
            .u8(b.material.wire_code())
            .u8(height_source_code(b.height_source))
            .u8(b.lod.wire_code())
            .optional_u32(b.name.map(|n| n.index()));
    }
    sections.push((section::BUILDINGS, core::mem::take(&mut w.buf)));

    if let Some(t) = &world.terrain {
        let mut w = Writer::new();
        w.f64(t.origin_x_m)
            .f64(t.origin_y_m)
            .f64(t.cell_x_m)
            .f64(t.cell_y_m)
            .u32(t.nx)
            .u32(t.ny)
            .u8(interpolation_code(t.interpolation))
            .optional_u32(t.source.map(|s| s.index()))
            .count(t.heights_m.len());
        for h in &t.heights_m {
            w.f64(*h);
        }
        sections.push((section::TERRAIN, core::mem::take(&mut w.buf)));
    }

    let mut w = Writer::new();
    w.count(world.signals.len());
    for plan in &world.signals {
        w.u32(plan.id.index())
            .u32(plan.junction.index())
            .f64(plan.cycle_s)
            .f64(plan.offset_s)
            .ids(
                plan.controlled.iter().map(|l| l.index()),
                plan.controlled.len(),
            )
            .count(plan.phases.len());
        for phase in &plan.phases {
            w.f64(phase.duration_s).count(phase.states.len());
            for s in &phase.states {
                w.u8(signal_state_code(*s));
            }
            w.optional_u32(phase.name.map(|n| n.index()));
        }
        w.count(plan.heads.len());
        for head in &plan.heads {
            w.u32(head.lane.index())
                .point(head.position)
                .u8(head.kind.wire_code())
                .u16(head.group);
        }
    }
    sections.push((section::SIGNALS, core::mem::take(&mut w.buf)));

    let mut w = Writer::new();
    w.count(world.sites.len());
    for s in &world.sites {
        w.u32(s.id.index())
            .optional_u32(s.node.map(|n| n.index()))
            .point(s.position)
            .f64(s.antenna_height_m)
            .f64(s.antenna_gain_dbi)
            .u8(s.kind.wire_code())
            .optional_u32(s.name.map(|n| n.index()));
    }
    sections.push((section::SITES, core::mem::take(&mut w.buf)));

    let mut w = Writer::new();
    w.count(world.landuse.len());
    for z in &world.landuse {
        w.u32(z.id.index())
            .points(&z.ring)
            .u8(z.class.wire_code())
            .u8(env_code(z.env))
            .optional_u32(z.name.map(|n| n.index()));
    }
    sections.push((section::LANDUSE, core::mem::take(&mut w.buf)));

    let provenance = serde_json::to_vec(&world.provenance)?;
    sections.push((section::PROVENANCE, provenance));

    Ok(assemble(&sections))
}

/// Lays out the container: header, table, then the payloads, each 8-byte aligned.
fn assemble(sections: &[(u16, Vec<u8>)]) -> Vec<u8> {
    let table_bytes = TABLE_ENTRY_BYTES * sections.len();
    let mut offset = align8(HEADER_BYTES + table_bytes);
    let mut out =
        Vec::with_capacity(offset + sections.iter().map(|s| s.1.len() + 8).sum::<usize>());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(sections.len() as u32).to_le_bytes());
    for (kind, payload) in sections {
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&(offset as u64).to_le_bytes());
        out.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        offset = align8(offset + payload.len());
    }
    for (_, payload) in sections {
        while out.len() % 8 != 0 {
            out.push(0);
        }
        out.extend_from_slice(payload);
    }
    while out.len() % 8 != 0 {
        out.push(0);
    }
    out
}

const fn align8(n: usize) -> usize {
    n.div_ceil(8) * 8
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// Reads a world back out of the versioned binary container.
///
/// # Errors
///
/// [`WorldError::Malformed`] for a bad magic, an unsupported version, a truncated
/// section or a bad enum code; whatever [`World::from_parts`] rejects otherwise —
/// including a content hash that does not match the geometry.
pub fn from_bytes(bytes: &[u8]) -> Result<World> {
    if bytes.len() < HEADER_BYTES {
        return Err(WorldError::Malformed {
            offset: 0,
            problem: format!("{} bytes is shorter than the 16-byte header", bytes.len()),
        });
    }
    if bytes[..8] != MAGIC {
        return Err(WorldError::Malformed {
            offset: 0,
            problem: format!(
                "bad magic {:02x?}, expected {:02x?} (\"V2XWWRLD\")",
                &bytes[..8],
                MAGIC
            ),
        });
    }
    let mut head = Reader::new(&bytes[8..HEADER_BYTES], 8);
    let version = head.u16()?;
    if version != FORMAT_VERSION {
        return Err(WorldError::Malformed {
            offset: 8,
            problem: format!(
                "format version {version}, this build writes and reads {FORMAT_VERSION}"
            ),
        });
    }
    let _reserved = head.u16()?;
    let section_count = head.u32()? as usize;

    let table_end = HEADER_BYTES + TABLE_ENTRY_BYTES * section_count;
    if table_end > bytes.len() {
        return Err(WorldError::Malformed {
            offset: HEADER_BYTES,
            problem: format!("section table of {section_count} entries runs past the file"),
        });
    }
    let mut table = Reader::new(&bytes[HEADER_BYTES..table_end], HEADER_BYTES);
    let mut found: BTreeMap<u16, (usize, usize)> = BTreeMap::new();
    for _ in 0..section_count {
        let kind = table.u16()?;
        let _flags = table.u16()?;
        let _reserved = table.u32()?;
        let offset = table.u64()? as usize;
        let length = table.u64()? as usize;
        if offset + length > bytes.len() {
            return Err(WorldError::Malformed {
                offset,
                problem: format!(
                    "section {kind} spans [{offset}, {}) past the file",
                    offset + length
                ),
            });
        }
        // A duplicate kind keeps the first: a writer never emits one, and picking a rule
        // beats undefined behaviour.
        found.entry(kind).or_insert((offset, length));
    }

    let slice = |kind: u16| -> Option<Reader<'_>> {
        found
            .get(&kind)
            .map(|(o, l)| Reader::new(&bytes[*o..*o + *l], *o))
    };
    let require = |kind: u16, name: &str| -> Result<Reader<'_>> {
        slice(kind).ok_or_else(|| WorldError::Malformed {
            offset: HEADER_BYTES,
            problem: format!("no {name} section"),
        })
    };

    let mut r = require(section::HEADER, "header")?;
    let origin = GeoOrigin {
        lat_deg: r.f64()?,
        lon_deg: r.f64()?,
        alt_m: r.f64()?,
    };
    let bbox = Bbox {
        min: r.point()?,
        max: r.point()?,
    };
    let default_env = env_from(r.u8()?).ok_or_else(|| WorldError::Malformed {
        offset: r.base + r.at,
        problem: "unknown environment class".to_string(),
    })?;
    let index_options = crate::index::IndexOptions {
        lane_grid_cell_m: r.f64()?,
        max_lane_grid_cells: r.u64()? as usize,
    };
    let mut content_hash = [0u8; 32];
    content_hash.copy_from_slice(r.take(32)?);

    let mut r = require(section::SYMBOLS, "symbols")?;
    let n = r.count()?;
    let mut strings = Vec::with_capacity(n.min(1 << 16));
    for _ in 0..n {
        strings.push(r.text()?);
    }
    let symbols = SymbolTable::from(strings);

    let mut r = require(section::LANES, "lanes")?;
    let n = r.count()?;
    let mut lanes = Vec::with_capacity(n.min(1 << 20));
    for _ in 0..n {
        let id = LaneId::new(r.u32()?);
        let edge = EdgeId::new(r.u32()?);
        let junction = r.optional_u32()?.map(JunctionId::new);
        let index = r.u8()?;
        let kind = LaneKind::from_wire_code(r.u8()?).ok_or_else(|| WorldError::Malformed {
            offset: r.base + r.at,
            problem: format!("unknown lane kind on lane {id}"),
        })?;
        let width_m = r.f64()?;
        let speed_limit_mps = r.f64()?;
        let allowed = ClassMask::from_bits(r.u16()?);
        let centreline = r.points()?;
        lanes.push(Lane::new(
            id,
            edge,
            junction,
            index,
            kind,
            centreline,
            width_m,
            speed_limit_mps,
            allowed,
        )?);
    }

    let mut r = require(section::EDGES, "edges")?;
    let n = r.count()?;
    let mut edges = Vec::with_capacity(n.min(1 << 20));
    for _ in 0..n {
        let id = EdgeId::new(r.u32()?);
        let from = JunctionId::new(r.u32()?);
        let to = JunctionId::new(r.u32()?);
        let lane_ids = r.ids(LaneId::new)?;
        let name = r.optional_u32()?.map(SymbolId::new);
        let road_class = road_class_from(r.u8()?).ok_or_else(|| WorldError::Malformed {
            offset: r.base + r.at,
            problem: format!("unknown road class on edge {id}"),
        })?;
        edges.push(Edge {
            id,
            from,
            to,
            lanes: lane_ids,
            name,
            road_class,
        });
    }

    let mut r = require(section::JUNCTIONS, "junctions")?;
    let n = r.count()?;
    let mut junctions = Vec::with_capacity(n.min(1 << 20));
    for _ in 0..n {
        let id = JunctionId::new(r.u32()?);
        let position = r.point()?;
        let shape = r.points()?;
        let incoming = r.ids(LaneId::new)?;
        let outgoing = r.ids(LaneId::new)?;
        let internal = r.ids(LaneId::new)?;
        let control_code = r.u8()?;
        let plan = r.optional_u32()?.map(SignalId::new);
        let control = match (control_code, plan) {
            (0, _) => JunctionControl::Uncontrolled,
            (1, _) => JunctionControl::Priority,
            (2, Some(plan)) => JunctionControl::Signalised { plan },
            (3, _) => JunctionControl::Stop,
            (4, _) => JunctionControl::Yield,
            (5, _) => JunctionControl::Roundabout,
            (code, _) => {
                return Err(WorldError::Malformed {
                    offset: r.base + r.at,
                    problem: format!("junction {id} has control code {code} without a plan"),
                });
            }
        };
        let rows = r.count()?;
        let words = r.count()?;
        let mut foes = Vec::with_capacity(words.min(1 << 16));
        for _ in 0..words {
            foes.push(r.u64()?);
        }
        let mut response = Vec::with_capacity(words.min(1 << 16));
        for _ in 0..words {
            response.push(r.u64()?);
        }
        let conflicts = ConflictMatrix::from_raw(rows, foes, response).ok_or_else(|| {
            WorldError::Malformed {
                offset: r.base + r.at,
                problem: format!("junction {id} conflict matrix has the wrong word count"),
            }
        })?;
        let name = r.optional_u32()?.map(SymbolId::new);
        junctions.push(Junction {
            id,
            position,
            shape,
            incoming,
            outgoing,
            internal,
            control,
            conflicts,
            name,
        });
    }

    let mut r = require(section::CONNECTIONS, "connections")?;
    let n = r.count()?;
    let mut connections = Vec::with_capacity(n.min(1 << 20));
    for _ in 0..n {
        let from_lane = LaneId::new(r.u32()?);
        let to_lane = LaneId::new(r.u32()?);
        let via = r.optional_u32()?.map(LaneId::new);
        let direction = turn_from(r.u8()?).ok_or_else(|| WorldError::Malformed {
            offset: r.base + r.at,
            problem: "unknown turn direction".to_string(),
        })?;
        let permitted = r.bool()?;
        connections.push(Connection {
            from_lane,
            to_lane,
            via,
            direction,
            permitted,
        });
    }

    let mut r = require(section::CROSSINGS, "crossings")?;
    let n = r.count()?;
    let mut crossings = Vec::with_capacity(n.min(1 << 20));
    for _ in 0..n {
        crossings.push(Crossing {
            id: CrossingId::new(r.u32()?),
            junction: JunctionId::new(r.u32()?),
            from: r.point()?,
            to: r.point()?,
            width_m: r.f64()?,
            priority: r.bool()?,
        });
    }

    let mut r = require(section::BUILDINGS, "buildings")?;
    let n = r.count()?;
    let mut buildings = Vec::with_capacity(n.min(1 << 20));
    for _ in 0..n {
        let id = BuildingId::new(r.u32()?);
        let footprint = r.points()?;
        let hole_count = r.count()?;
        let mut holes = Vec::with_capacity(hole_count.min(1 << 12));
        for _ in 0..hole_count {
            holes.push(r.points()?);
        }
        let height_m = r.f64()?;
        let min_height_m = r.f64()?;
        let base_z_m = r.f64()?;
        let levels = r
            .optional_u32()?
            .map(|v| u16::try_from(v).unwrap_or(u16::MAX));
        let material =
            MaterialClass::from_wire_code(r.u8()?).ok_or_else(|| WorldError::Malformed {
                offset: r.base + r.at,
                problem: format!("unknown material on building {id}"),
            })?;
        let height_source = height_source_from(r.u8()?).ok_or_else(|| WorldError::Malformed {
            offset: r.base + r.at,
            problem: format!("unknown height source on building {id}"),
        })?;
        let lod = LodHint::from_wire_code(r.u8()?).ok_or_else(|| WorldError::Malformed {
            offset: r.base + r.at,
            problem: format!("unknown level-of-detail hint on building {id}"),
        })?;
        let name = r.optional_u32()?.map(SymbolId::new);
        buildings.push(Building {
            id,
            footprint,
            holes,
            height_m,
            min_height_m,
            base_z_m,
            levels,
            material,
            height_source,
            lod,
            name,
        });
    }

    let terrain = match slice(section::TERRAIN) {
        None => None,
        Some(mut r) => {
            let origin_x_m = r.f64()?;
            let origin_y_m = r.f64()?;
            let cell_x_m = r.f64()?;
            let cell_y_m = r.f64()?;
            let nx = r.u32()?;
            let ny = r.u32()?;
            let interpolation =
                interpolation_from(r.u8()?).ok_or_else(|| WorldError::Malformed {
                    offset: r.base + r.at,
                    problem: "unknown terrain interpolation".to_string(),
                })?;
            let source = r.optional_u32()?.map(SymbolId::new);
            let count = r.count()?;
            let mut heights = Vec::with_capacity(count.min(1 << 22));
            for _ in 0..count {
                heights.push(r.f64()?);
            }
            let mut t = Terrain::new(
                origin_x_m,
                origin_y_m,
                cell_x_m,
                cell_y_m,
                nx,
                ny,
                heights,
                interpolation,
            )?;
            t.source = source;
            Some(t)
        }
    };

    let mut r = require(section::SIGNALS, "signals")?;
    let n = r.count()?;
    let mut signals = Vec::with_capacity(n.min(1 << 20));
    for _ in 0..n {
        let id = SignalId::new(r.u32()?);
        let junction = JunctionId::new(r.u32()?);
        let cycle_s = r.f64()?;
        let offset_s = r.f64()?;
        let controlled = r.ids(LaneId::new)?;
        let phase_count = r.count()?;
        let mut phases = Vec::with_capacity(phase_count.min(1 << 12));
        for _ in 0..phase_count {
            let duration_s = r.f64()?;
            let state_count = r.count()?;
            let mut states = Vec::with_capacity(state_count.min(1 << 12));
            for _ in 0..state_count {
                states.push(
                    signal_state_from(r.u8()?).ok_or_else(|| WorldError::Malformed {
                        offset: r.base + r.at,
                        problem: format!("unknown signal state in plan {id}"),
                    })?,
                );
            }
            let name = r.optional_u32()?.map(SymbolId::new);
            phases.push(SignalPhase {
                duration_s,
                states,
                name,
            });
        }
        let head_count = r.count()?;
        let mut heads = Vec::with_capacity(head_count.min(1 << 16));
        for _ in 0..head_count {
            heads.push(SignalHead {
                lane: LaneId::new(r.u32()?),
                position: r.point()?,
                kind: signal_head_kind_from(r.u8()?).ok_or_else(|| WorldError::Malformed {
                    offset: r.base + r.at,
                    problem: format!("unknown signal head kind in plan {id}"),
                })?,
                group: r.u16()?,
            });
        }
        signals.push(SignalPlan {
            id,
            junction,
            cycle_s,
            offset_s,
            controlled,
            phases,
            heads,
        });
    }

    let mut r = require(section::SITES, "sites")?;
    let n = r.count()?;
    let mut sites = Vec::with_capacity(n.min(1 << 20));
    for _ in 0..n {
        sites.push(Site {
            id: SiteId::new(r.u32()?),
            node: r.optional_u32()?.map(NodeId::new),
            position: r.point()?,
            antenna_height_m: r.f64()?,
            antenna_gain_dbi: r.f64()?,
            kind: site_kind_from(r.u8()?).ok_or_else(|| WorldError::Malformed {
                offset: r.base + r.at,
                problem: "unknown site kind".to_string(),
            })?,
            name: r.optional_u32()?.map(SymbolId::new),
        });
    }

    let mut r = require(section::LANDUSE, "landuse")?;
    let n = r.count()?;
    let mut landuse = Vec::with_capacity(n.min(1 << 20));
    for _ in 0..n {
        let id = ZoneId::new(r.u32()?);
        let ring = r.points()?;
        let class = LanduseClass::from_wire_code(r.u8()?).ok_or_else(|| WorldError::Malformed {
            offset: r.base + r.at,
            problem: format!("unknown land-use class on zone {id}"),
        })?;
        let env = env_from(r.u8()?).ok_or_else(|| WorldError::Malformed {
            offset: r.base + r.at,
            problem: format!("unknown environment class on zone {id}"),
        })?;
        let name = r.optional_u32()?.map(SymbolId::new);
        landuse.push(LanduseZone {
            id,
            ring,
            class,
            env,
            name,
        });
    }

    let mut r = require(section::PROVENANCE, "provenance")?;
    let provenance: WorldProvenance = serde_json::from_slice(r.take(r.bytes.len())?)?;

    World::from_parts(WorldParts {
        origin,
        bbox,
        roads: RoadNetwork::new(lanes, edges, junctions, connections, crossings)?,
        buildings,
        terrain,
        signals,
        sites,
        landuse,
        default_env,
        symbols,
        provenance,
        content_hash,
        index_options,
    })
}

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

/// The world as JSON, on one line — for tests, diffs and `jq`.
///
/// This is the whole model, not the UI's mirror form: [`crate::serde_vwp::to_json`]
/// writes that one.
pub fn to_json(world: &World) -> Result<String> {
    Ok(serde_json::to_string(world)?)
}

/// The world as indented JSON.
pub fn to_json_pretty(world: &World) -> Result<String> {
    Ok(serde_json::to_string_pretty(world)?)
}

/// Reads a world back from the JSON form, validating it and checking its content hash.
///
/// # Errors
///
/// [`WorldError::Json`] if the text is not the expected shape, or whatever
/// [`World::from_parts`] rejects.
pub fn from_json(text: &str) -> Result<World> {
    let world: World = serde_json::from_str(text)?;
    World::from_parts(world.to_parts())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procedural::{GridParams, grid};

    fn world() -> World {
        grid(
            &GridParams::legacy().with_size(2, 2),
            &crate::ImportOptions::default().imported_at("2026-09-18T00:00:00Z"),
        )
        .unwrap()
    }

    #[test]
    fn enum_codes_round_trip() {
        for c in [
            RoadClass::Motorway,
            RoadClass::Trunk,
            RoadClass::Primary,
            RoadClass::Secondary,
            RoadClass::Tertiary,
            RoadClass::Residential,
            RoadClass::Living,
            RoadClass::Service,
            RoadClass::Link,
            RoadClass::Footway,
            RoadClass::Cycleway,
            RoadClass::Path,
            RoadClass::Internal,
            RoadClass::Unclassified,
        ] {
            assert_eq!(road_class_from(road_class_code(c)), Some(c));
        }
        assert_eq!(road_class_from(14), None);
        for t in [
            TurnDirection::Straight,
            TurnDirection::Left,
            TurnDirection::Right,
            TurnDirection::SlightLeft,
            TurnDirection::SlightRight,
            TurnDirection::UTurn,
        ] {
            assert_eq!(turn_from(turn_code(t)), Some(t));
        }
        assert_eq!(turn_from(6), None);
        for s in [
            SignalState::Red,
            SignalState::RedAmber,
            SignalState::Amber,
            SignalState::Green,
            SignalState::GreenYield,
            SignalState::FlashingAmber,
            SignalState::Off,
        ] {
            assert_eq!(signal_state_from(signal_state_code(s)), Some(s));
        }
        assert_eq!(signal_state_from(7), None);
        for e in [
            EnvClass::Urban,
            EnvClass::Suburban,
            EnvClass::Highway,
            EnvClass::Rural,
        ] {
            assert_eq!(env_from(env_code(e)), Some(e));
        }
        assert_eq!(env_from(4), None);
        for h in [
            HeightSource::Tagged,
            HeightSource::FromLevels,
            HeightSource::Defaulted,
            HeightSource::FromParts,
        ] {
            assert_eq!(height_source_from(height_source_code(h)), Some(h));
        }
        assert_eq!(height_source_from(4), None);
        for i in [Interpolation::Bilinear, Interpolation::Nearest] {
            assert_eq!(interpolation_from(interpolation_code(i)), Some(i));
        }
        assert_eq!(signal_head_kind_from(3), Some(SignalHeadKind::Transit));
        assert_eq!(signal_head_kind_from(4), None);
        assert_eq!(site_kind_from(2), Some(SiteKind::Other));
        assert_eq!(site_kind_from(3), None);
    }

    #[test]
    fn container_header_is_as_documented() {
        let bytes = to_bytes(&world()).unwrap();
        assert_eq!(&bytes[..8], &MAGIC);
        assert_eq!(u16::from_le_bytes([bytes[8], bytes[9]]), FORMAT_VERSION);
        assert_eq!(u16::from_le_bytes([bytes[10], bytes[11]]), 0, "reserved");
        let sections = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]) as usize;
        assert_eq!(
            sections, 12,
            "every section but the terrain this world lacks"
        );
        // Every section is 8-byte aligned and inside the file.
        for i in 0..sections {
            let at = HEADER_BYTES + TABLE_ENTRY_BYTES * i;
            let offset = u64::from_le_bytes(bytes[at + 8..at + 16].try_into().unwrap()) as usize;
            let length = u64::from_le_bytes(bytes[at + 16..at + 24].try_into().unwrap()) as usize;
            assert_eq!(offset % 8, 0, "section {i} is 8-byte aligned");
            assert!(offset + length <= bytes.len());
        }
    }

    #[test]
    fn a_reader_skips_a_section_it_does_not_know() {
        // Forward compatibility: relabel the provenance section as a kind from the
        // future and the file must still fail *for the right reason* — the section it
        // needs is missing, not a parse error halfway through.
        let mut bytes = to_bytes(&world()).unwrap();
        let sections = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]) as usize;
        let mut relabelled = false;
        for i in 0..sections {
            let at = HEADER_BYTES + TABLE_ENTRY_BYTES * i;
            if u16::from_le_bytes([bytes[at], bytes[at + 1]]) == section::PROVENANCE {
                bytes[at..at + 2].copy_from_slice(&999u16.to_le_bytes());
                relabelled = true;
            }
        }
        assert!(relabelled);
        let err = from_bytes(&bytes).unwrap_err();
        assert!(
            err.to_string().contains("no provenance section"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn a_truncated_file_is_an_error_not_a_panic() {
        let bytes = to_bytes(&world()).unwrap();
        // The last section is the provenance blob, padded to a multiple of four, so
        // cutting eight bytes is guaranteed to cut into the blob itself rather than into
        // padding a reader is entitled to ignore.
        for cut in [0, 4, 16, 40, bytes.len() / 2, bytes.len() - 8] {
            assert!(from_bytes(&bytes[..cut]).is_err(), "cut at {cut}");
        }
    }

    #[test]
    fn json_round_trip_keeps_every_bit() {
        let w = world();
        let text = to_json_pretty(&w).unwrap();
        assert!(text.contains("\"content_hash\""));
        let back = from_json(&text).unwrap();
        assert_eq!(back, w);
        // The hex form of the hash survives, rather than 32 integers.
        assert!(text.contains(&v2xw_core::hash::hex_encode(&w.content_hash)));
    }
}
