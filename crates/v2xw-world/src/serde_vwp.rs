//! The `vwp-world/1` payload — docs/protocol/vwp-v1.md §4.
//!
//! This is what the UI fetches with `GET /world/{64-hex-sha256}.vwb`, and what the
//! TypeScript client in `ui/packages/protocol` decodes. The layout is fixed by the
//! specification and is reproduced here byte for byte: a 16-byte file header, a 192-byte
//! directory, struct-of-array lane and building tables, parallel `f32` point arrays,
//! array-of-struct sections for junctions, signals, sites, crossings and land use, a
//! symbol table and a JSON provenance blob.
//!
//! # What it is not
//!
//! It is not the engine's own format — [`crate::serde_native`] is. This one drops
//! everything a renderer does not need: lane connectivity and conflict matrices
//! (§4.3's `DECISION`), building holes (§4.4's `DECISION`), the terrain grid, the
//! arc-length tables. Those drops are recorded in the world provenance, as §4.4 requires.
//!
//! # Two digests, on purpose: the geometry digest and the payload digest
//!
//! * The **geometry digest**, [`World::content_hash`] ([`crate::hash`], invariant I-W2):
//!   a hash of the *model*, in quantised integers, independent of any wire format. It
//!   identifies the world, and it is what the run manifest and the provenance record.
//! * The **payload digest**, [`WorldPayload::content_hash`]: SHA-256 of this file's body,
//!   as §4.2 defines it. It addresses the payload — it is the `{hash}` of
//!   `GET /world/{hash}.vwb` ([`WorldPayload::url_path`]) — and it is the value
//!   `Hello.world_hash` carries, per §4.2's `MUST` ("MUST equal the URL hash and
//!   `Hello.world_hash`") and conformance item W1 of §10.5. §3.1.1's note column glosses
//!   `Hello.world_hash` as `World.content_hash`, which is the *geometry* digest and a
//!   different number; that gloss contradicts the `MUST` and is recorded as a
//!   specification defect in §4.2's amendment note.
//!
//! They are different numbers because they hash different things, and both are needed.
//! Nothing in this crate returns one where the other belongs, and
//! `the_payload_url_is_keyed_by_the_payload_digest` in `tests/world.rs` pins that.
//!
//! §4.2 asks for the digest of the body while storing it *inside* the body, which cannot
//! hold literally; this writer resolves it the same way the TypeScript client does —
//! **the 32 bytes of the `content_hash` field are zero while the body is hashed** — so
//! the two implementations agree. vwp-v1 §4.2 has been amended (2026-09-18) to state that
//! rule, since it is what two independent implementations already do.
//!
//! # Precision
//!
//! Every coordinate is quantised ([`crate::quant`]) and then narrowed to `f32`, which is
//! what the format stores. `f32` holds a millimetre grid out to
//! [`crate::quant::F32_MM_GRID_LIMIT_M`] = 16 384 m (16 km) from the origin; beyond that
//! the payload is coarser than the model, and [`WorldPayload::precision_warnings`] says
//! so — for **every** narrowed column, not just lane centrelines — rather than letting it
//! pass silently.

use v2xw_core::geom::Vec3;

use crate::error::{Result, WorldError};
use crate::model::{Building, Lane, SignalHead, SignalPlan, World};
use crate::quant::{
    F32_MM_GRID_LIMIT_M, Q_DB, Q_HEIGHT_M, Q_POSITION_M, Q_SPEED_MPS, quantise_f32,
};

/// `magic` of §4.1: `0x444C5756`, whose little-endian wire bytes are `V W L D`.
pub const MAGIC: u32 = 0x444C_5756;

/// `version` of §4.1.
pub const VERSION: u16 = 1;

/// The file header is 16 bytes; the body starts at file offset 16 (§4.1).
pub const HEADER_BYTES: usize = 16;

/// The directory prefix is 192 bytes, body-relative (§4.2).
pub const DIRECTORY_BYTES: usize = 192;

/// Bytes per lane row (§4.3).
pub const LANE_STRIDE: usize = 36;
/// Bytes per building row (§4.4).
pub const BUILDING_STRIDE: usize = 28;
/// Bytes per junction record (§4.5).
pub const JUNCTION_STRIDE: usize = 24;
/// Bytes per signal record (§4.5).
pub const SIGNAL_STRIDE: usize = 28;
/// Bytes per site record (§4.5).
pub const SITE_STRIDE: usize = 32;
/// Bytes per crossing record (§4.5).
pub const CROSSING_STRIDE: usize = 28;
/// Bytes per land-use record (§4.5).
pub const LANDUSE_STRIDE: usize = 16;

/// The media type of the binary form (§4).
pub const CONTENT_TYPE: &str = "application/vnd.v2xw.world.v1";

/// The `u32` "absent" sentinel of §0.
const U32_NONE: u32 = 0xFFFF_FFFF;
/// The `u16` "absent" sentinel of §0.
const U16_NONE: u16 = 0xFFFF;

/// How many entries [`WorldPayload::precision_warnings`] holds before it stops
/// collecting.
///
/// A world past the `f32` limit is past it for most of its points, so the list is a
/// sample, not an inventory: sixteen is enough to see which sections are affected and how
/// far out they reach.
pub const MAX_PRECISION_WARNINGS: usize = 16;

/// The writer's precision-warning collector: one entry per narrowed **column**, naming
/// the first object in that column that goes past the `f32` millimetre-grid limit.
///
/// One entry per column rather than per value, because a world that is too big is too big
/// for most of its points: a per-value list would be filled by the first section written
/// (lane centrelines) and would never mention the buildings, junctions, sites or crossings
/// that are equally affected — which is exactly how the old warning managed to cover only
/// two columns.
#[derive(Debug, Default)]
struct PrecisionWarnings {
    seen: Vec<&'static str>,
    out: Vec<String>,
}

impl PrecisionWarnings {
    /// Records `value`'s column if the value is too far from the origin for `f32` to hold
    /// the millimetre grid and that column has not been recorded yet.
    ///
    /// `what` is a closure so that the message — which names the offending object — is
    /// formatted only when a warning is actually raised. The writer calls this once per
    /// narrowed coordinate, which on a city-sized world is hundreds of thousands of times.
    fn check(&mut self, column: &'static str, value: f64, what: impl FnOnce() -> String) {
        if value.abs() > F32_MM_GRID_LIMIT_M
            && self.out.len() < MAX_PRECISION_WARNINGS
            && !self.seen.contains(&column)
        {
            self.seen.push(column);
            self.out.push(format!(
                "{} = {value} m is beyond {F32_MM_GRID_LIMIT_M} m, where f32 no longer \
                 holds the {Q_POSITION_M} m grid",
                what()
            ));
        }
    }
}

/// The §4.5 junction `lane_count` column for a junction that touches `touching` lanes.
///
/// # Errors
///
/// [`WorldError::Unrepresentable`] if the count cannot be *stated*. The column is a `u16`
/// and §0 reserves `0xFFFF` for "absent", so the largest number this column can say is
/// `0xFFFE`. The writer used to say `u16::try_from(touching).unwrap_or(U16_NONE)`, which
/// turned an overflow into the "absent" sentinel — so "touches 65 535 lanes" and "not
/// stated" became the same bytes. Unreachable on any realistic world, and an error rather
/// than a sentinel collision precisely because nobody would ever look.
fn lane_count_column(touching: usize, junction: u32) -> Result<u16> {
    if touching >= usize::from(U16_NONE) {
        return Err(WorldError::Unrepresentable {
            what: format!("junction {junction} lane_count"),
            format: "vwp-world/1",
            problem: format!(
                "the junction touches {touching} lanes; §4.5 stores the count in a u16 whose \
                 {U16_NONE:#06x} is §0's \"absent\" sentinel, so at most {} can be stated",
                U16_NONE - 1
            ),
        });
    }
    Ok(touching as u16)
}

/// Converts a size, offset or count to the `u32` the format stores.
///
/// # Errors
///
/// [`WorldError::Unrepresentable`] if it does not fit. §4.1 and §4.2 store `body_len`,
/// every `off_*` and every count as a `u32`, so a payload above 4 GiB cannot be
/// described; an `as u32` cast would wrap and write a structurally corrupt file that
/// every reader would then mis-parse, which is worse than refusing to write it.
fn u32_field(what: &str, value: usize) -> Result<u32> {
    u32::try_from(value).map_err(|_| WorldError::Unrepresentable {
        what: what.to_string(),
        format: "vwp-world/1",
        problem: format!("{value} does not fit the u32 the §4.1/§4.2 header stores"),
    })
}

/// Directory field offsets, body-relative (§4.2).
mod dir {
    pub const CONTENT_HASH: usize = 0;
    pub const ORIGIN_LAT: usize = 32;
    pub const ORIGIN_LON: usize = 40;
    pub const ORIGIN_ALT: usize = 48;
    pub const BBOX_MIN_X: usize = 56;
    pub const BBOX_MIN_Y: usize = 64;
    pub const BBOX_MAX_X: usize = 72;
    pub const BBOX_MAX_Y: usize = 80;
    pub const BBOX_MIN_Z: usize = 88;
    pub const BBOX_MAX_Z: usize = 92;
    pub const LANE_COUNT: usize = 96;
    pub const LANE_POINT_TOTAL: usize = 100;
    pub const BUILDING_COUNT: usize = 104;
    pub const RING_POINT_TOTAL: usize = 108;
    pub const OFF_LANES: usize = 112;
    pub const OFF_LANE_POINTS: usize = 116;
    pub const OFF_BUILDINGS: usize = 120;
    pub const OFF_RING_POINTS: usize = 124;
    pub const JUNCTION_COUNT: usize = 128;
    pub const OFF_JUNCTIONS: usize = 132;
    pub const SIGNAL_COUNT: usize = 136;
    pub const OFF_SIGNALS: usize = 140;
    pub const SITE_COUNT: usize = 144;
    pub const OFF_SITES: usize = 148;
    pub const OFF_STRINGS: usize = 152;
    pub const CROSSING_COUNT: usize = 156;
    pub const OFF_CROSSINGS: usize = 160;
    pub const LANDUSE_COUNT: usize = 164;
    pub const OFF_LANDUSE: usize = 168;
    pub const OFF_PROVENANCE_JSON: usize = 172;
    pub const PROVENANCE_JSON_BYTES: usize = 176;
}

/// A written `vwp-world/1` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldPayload {
    /// The complete file: 16-byte header followed by the body.
    pub bytes: Vec<u8>,
    /// The **payload digest**: SHA-256 of the body with the `content_hash` field zeroed.
    ///
    /// This is the `{hash}` of `GET /world/{hash}.vwb` ([`WorldPayload::url_path`]) and
    /// the value `Hello.world_hash` carries (§4.2's `MUST`, conformance W1). It is *not*
    /// [`World::content_hash`], which is the geometry digest that identifies the world in
    /// the run manifest; see this module's header.
    pub content_hash: [u8; 32],
    /// Coordinates that `f32` cannot hold on the millimetre grid, if any.
    ///
    /// Empty for every world that stays within [`crate::quant::F32_MM_GRID_LIMIT_M`] =
    /// 16 384 m of its origin, which is every world under 16 km across (the origin is the
    /// bounding box's south-west corner, D6). A non-empty list is not an error — the
    /// payload is still valid and still deterministic — but it means the UI sees a coarser
    /// grid than the engine, and whoever built a world that big should know.
    ///
    /// Every narrowed column is checked, not only lane centrelines: lane `z`, building and
    /// land-use ring points, junction positions, signal-head positions, sites (including
    /// the antenna height) and crossings all narrow to `f32` too, and a world whose
    /// buildings or infrastructure reach past the limit while its lanes do not was
    /// silently losing millimetres with an empty list here. The list is capped at
    /// [`MAX_PRECISION_WARNINGS`] entries, because one per point would be a book.
    pub precision_warnings: Vec<String>,
}

impl WorldPayload {
    /// The path this payload is served from, `/world/<64 hex>.vwb` (§4).
    pub fn url_path(&self) -> String {
        format!(
            "/world/{}.vwb",
            v2xw_core::hash::hex_encode(&self.content_hash)
        )
    }

    /// The content hash as lower-case hex.
    pub fn content_hash_hex(&self) -> String {
        v2xw_core::hash::hex_encode(&self.content_hash)
    }

    /// The body alone, without the 16-byte file header.
    pub fn body(&self) -> &[u8] {
        &self.bytes[HEADER_BYTES..]
    }
}

/// Little-endian scalar writes into a preallocated buffer.
///
/// Every offset is computed up front from the section sizes, exactly as the reference
/// TypeScript encoder does, so the two produce identical bytes.
fn put_u8(buf: &mut [u8], at: usize, v: u8) {
    buf[at] = v;
}

fn put_u16(buf: &mut [u8], at: usize, v: u16) {
    buf[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

fn put_u32(buf: &mut [u8], at: usize, v: u32) {
    buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn put_f32(buf: &mut [u8], at: usize, v: f32) {
    buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn put_f64(buf: &mut [u8], at: usize, v: f64) {
    buf[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

const fn ceil4(n: usize) -> usize {
    n.div_ceil(4) * 4
}

/// The `StrTable` of §2.5: `n`, `blob_bytes`, `n + 1` offsets, then the UTF-8 blob padded
/// to a multiple of four.
fn encode_str_table(strings: &[String]) -> Vec<u8> {
    let blob_bytes: usize = strings.iter().map(String::len).sum();
    let total = 8 + 4 * (strings.len() + 1) + ceil4(blob_bytes);
    let mut out = vec![0u8; total];
    put_u32(&mut out, 0, strings.len() as u32);
    put_u32(&mut out, 4, blob_bytes as u32);
    let mut cursor = 0usize;
    for (i, s) in strings.iter().enumerate() {
        put_u32(&mut out, 8 + 4 * i, cursor as u32);
        cursor += s.len();
    }
    put_u32(&mut out, 8 + 4 * strings.len(), blob_bytes as u32);
    let mut at = 8 + 4 * (strings.len() + 1);
    for s in strings {
        out[at..at + s.len()].copy_from_slice(s.as_bytes());
        at += s.len();
    }
    out
}

/// Every signal head in the world, flattened to the one-record-per-head form §4.5 wants.
///
/// Order: by plan id, then by the head's position in its plan — deterministic, and it
/// keeps a junction's heads together, which is what a renderer wants for batching.
fn heads_of(world: &World) -> Vec<(&SignalPlan, &SignalHead)> {
    let mut out = Vec::new();
    for plan in &world.signals {
        for head in &plan.heads {
            out.push((plan, head));
        }
    }
    out
}

/// The street name of a lane: its edge's name, or the empty string.
fn lane_name_symbol(world: &World, lane: &Lane) -> u32 {
    world
        .roads
        .try_edge(lane.edge)
        .and_then(|e| e.name)
        .map_or(0, |s| s.index())
}

/// Writes the `vwp-world/1` payload of a world (§4).
///
/// # What goes in, and how
///
/// * **Lanes**: all of them, junction connectors included — a connector carries its
///   junction in the `junction_id` column and `lane_type = 5`, exactly as §4.3 says. The
///   street name is the lane's *edge's* name.
/// * **Signals**: one record per physical head, so a controller with heads on four
///   approaches produces four records that share its `signal_id`. §4.5's `signal_id` is
///   the controller's id (03-interfaces.md §1: "a traffic signal (one controller with a
///   phase plan)"), and the renderer keys a lantern by its row, not by that id.
/// * **Junctions**: `lane_count` is the number of lanes the junction touches — incoming,
///   outgoing and internal.
/// * **Land use**: its rings are appended to the building ring arrays, which §4.4 shares
///   between the two.
/// * **Strings**: the world's own symbol table is written verbatim, so a payload string
///   id *is* a [`crate::model::SymbolId`]. §4 scopes the table to the file, so nothing
///   depends on that, but it makes a payload and its world easy to compare.
/// * **Dropped**: building holes, the terrain grid, the lane graph, the conflict
///   matrices and the signal *plans* — §4.3 and §4.4 say so, and the world provenance
///   records it.
///
/// # Errors
///
/// [`WorldError::Unrepresentable`] when the world cannot be written as a `vwp-world/1`
/// file at all:
///
/// * narrowing a lane centreline to `f32` would make two successive points equal, which
///   §4.3 forbids ("successive points MUST differ by ≥ 1 mm");
/// * a junction touches `0xFFFF` lanes or more, which §4.5's `u16` `lane_count` cannot
///   state without colliding with §0's "absent" sentinel;
/// * the body, an `off_*` offset or a count exceeds `u32::MAX` — a payload above 4 GiB,
///   which §4.1's and §4.2's `u32` fields cannot describe.
///
/// The last two are unreachable on any realistic world (Manhattan's payload is 2 MB), and
/// that is the point: they fail loudly rather than writing a file whose header lies.
///
/// [`WorldError::Json`] if the provenance will not serialise.
pub fn write(world: &World) -> Result<WorldPayload> {
    let lanes = world.roads.lanes();
    let lane_count = lanes.len();
    let lane_point_total: usize = lanes.iter().map(Lane::point_count).sum();
    let buildings = &world.buildings;
    let building_count = buildings.len();
    let building_ring_total: usize = buildings.iter().map(|b| b.open_ring().len()).sum();
    let landuse_ring_total: usize = world.landuse.iter().map(|z| z.open_ring().len()).sum();
    let ring_point_total = building_ring_total + landuse_ring_total;
    let junctions = world.roads.junctions();
    let heads = heads_of(world);
    let crossings = world.roads.crossings();

    let str_table = encode_str_table(world.symbols.strings());
    let provenance_json = serde_json::to_vec(&world.provenance.to_wire_json())?;

    // Section offsets, in the order §4.2's directory lists them. `0` means absent (§2.2),
    // and every section is a multiple of four bytes long, so nothing needs padding until
    // the provenance blob.
    let mut at = DIRECTORY_BYTES;
    let off_lanes = if lane_count > 0 { at } else { 0 };
    at += LANE_STRIDE * lane_count;
    let off_lane_points = if lane_point_total > 0 { at } else { 0 };
    at += 12 * lane_point_total;
    let off_buildings = if building_count > 0 { at } else { 0 };
    at += BUILDING_STRIDE * building_count;
    let off_ring_points = if ring_point_total > 0 { at } else { 0 };
    at += 8 * ring_point_total;
    let off_junctions = if !junctions.is_empty() { at } else { 0 };
    at += JUNCTION_STRIDE * junctions.len();
    let off_signals = if !heads.is_empty() { at } else { 0 };
    at += SIGNAL_STRIDE * heads.len();
    let off_sites = if !world.sites.is_empty() { at } else { 0 };
    at += SITE_STRIDE * world.sites.len();
    let off_crossings = if !crossings.is_empty() { at } else { 0 };
    at += CROSSING_STRIDE * crossings.len();
    let off_landuse = if !world.landuse.is_empty() { at } else { 0 };
    at += LANDUSE_STRIDE * world.landuse.len();
    let off_strings = at;
    at += str_table.len();
    let off_provenance = if provenance_json.is_empty() { 0 } else { at };
    at += ceil4(provenance_json.len());
    let body_len = at;

    // §4.1/§4.2 — nothing the header or the directory holds may be silently truncated
    // into a `u32`. Checking `body_len` alone would be enough to bound every offset and
    // every count (each is either at most `body_len` or at most `body_len / 16`), but each
    // one goes through the same guard anyway: "provably in range" is an argument, and a
    // `try_from` is a fact. The check comes before the allocation, so an impossible
    // payload is refused rather than assembled.
    let body_len_u32 = u32_field("body_len", body_len)?;

    let mut file = vec![0u8; HEADER_BYTES + body_len];
    put_u32(&mut file, 0, MAGIC);
    put_u16(&mut file, 4, VERSION);
    put_u32(&mut file, 8, body_len_u32);
    put_u16(&mut file, 12, 0);
    let body = &mut file[HEADER_BYTES..];

    let mut warnings = PrecisionWarnings::default();

    put_f64(body, dir::ORIGIN_LAT, world.origin.lat_deg);
    put_f64(body, dir::ORIGIN_LON, world.origin.lon_deg);
    put_f64(body, dir::ORIGIN_ALT, world.origin.alt_m);
    put_f64(body, dir::BBOX_MIN_X, world.bbox.min.x);
    put_f64(body, dir::BBOX_MIN_Y, world.bbox.min.y);
    put_f64(body, dir::BBOX_MAX_X, world.bbox.max.x);
    put_f64(body, dir::BBOX_MAX_Y, world.bbox.max.y);
    warnings.check("bbox.z", world.bbox.min.z, || "bbox min z".to_string());
    warnings.check("bbox.z", world.bbox.max.z, || "bbox max z".to_string());
    put_f32(
        body,
        dir::BBOX_MIN_Z,
        quantise_f32(world.bbox.min.z, Q_HEIGHT_M),
    );
    put_f32(
        body,
        dir::BBOX_MAX_Z,
        quantise_f32(world.bbox.max.z, Q_HEIGHT_M),
    );
    put_u32(body, dir::LANE_COUNT, u32_field("lane_count", lane_count)?);
    put_u32(
        body,
        dir::LANE_POINT_TOTAL,
        u32_field("lane_point_total", lane_point_total)?,
    );
    put_u32(
        body,
        dir::BUILDING_COUNT,
        u32_field("building_count", building_count)?,
    );
    put_u32(
        body,
        dir::RING_POINT_TOTAL,
        u32_field("ring_point_total", ring_point_total)?,
    );
    put_u32(body, dir::OFF_LANES, u32_field("off_lanes", off_lanes)?);
    put_u32(
        body,
        dir::OFF_LANE_POINTS,
        u32_field("off_lane_points", off_lane_points)?,
    );
    put_u32(
        body,
        dir::OFF_BUILDINGS,
        u32_field("off_buildings", off_buildings)?,
    );
    put_u32(
        body,
        dir::OFF_RING_POINTS,
        u32_field("off_ring_points", off_ring_points)?,
    );
    put_u32(
        body,
        dir::JUNCTION_COUNT,
        u32_field("junction_count", junctions.len())?,
    );
    put_u32(
        body,
        dir::OFF_JUNCTIONS,
        u32_field("off_junctions", off_junctions)?,
    );
    put_u32(
        body,
        dir::SIGNAL_COUNT,
        u32_field("signal_count", heads.len())?,
    );
    put_u32(
        body,
        dir::OFF_SIGNALS,
        u32_field("off_signals", off_signals)?,
    );
    put_u32(
        body,
        dir::SITE_COUNT,
        u32_field("site_count", world.sites.len())?,
    );
    put_u32(body, dir::OFF_SITES, u32_field("off_sites", off_sites)?);
    put_u32(
        body,
        dir::OFF_STRINGS,
        u32_field("off_strings", off_strings)?,
    );
    put_u32(
        body,
        dir::CROSSING_COUNT,
        u32_field("crossing_count", crossings.len())?,
    );
    put_u32(
        body,
        dir::OFF_CROSSINGS,
        u32_field("off_crossings", off_crossings)?,
    );
    put_u32(
        body,
        dir::LANDUSE_COUNT,
        u32_field("landuse_count", world.landuse.len())?,
    );
    put_u32(
        body,
        dir::OFF_LANDUSE,
        u32_field("off_landuse", off_landuse)?,
    );
    put_u32(
        body,
        dir::OFF_PROVENANCE_JSON,
        u32_field("off_provenance_json", off_provenance)?,
    );
    put_u32(
        body,
        dir::PROVENANCE_JSON_BYTES,
        u32_field("provenance_json_bytes", provenance_json.len())?,
    );

    // §4.3 — lanes, struct-of-arrays: eleven columns, each laid out end to end.
    let l = lane_count;
    let mut point_cursor = 0usize;
    for (i, lane) in lanes.iter().enumerate() {
        put_u32(body, off_lanes + 4 * i, lane.id.index());
        put_u32(body, off_lanes + 4 * l + 4 * i, point_cursor as u32);
        put_u32(body, off_lanes + 8 * l + 4 * i, lane.point_count() as u32);
        put_u32(body, off_lanes + 12 * l + 4 * i, lane.edge.index());
        put_u32(
            body,
            off_lanes + 16 * l + 4 * i,
            lane.junction.map_or(U32_NONE, |j| j.index()),
        );
        put_u32(
            body,
            off_lanes + 20 * l + 4 * i,
            lane_name_symbol(world, lane),
        );
        put_f32(
            body,
            off_lanes + 24 * l + 4 * i,
            quantise_f32(lane.width_m, Q_POSITION_M),
        );
        put_f32(
            body,
            off_lanes + 28 * l + 4 * i,
            quantise_f32(lane.speed_limit_mps, Q_SPEED_MPS),
        );
        put_u16(body, off_lanes + 32 * l + 2 * i, lane.allowed.bits());
        put_u8(body, off_lanes + 34 * l + i, lane.kind.wire_code());
        put_u8(body, off_lanes + 35 * l + i, lane.index);

        let t = lane_point_total;
        let mut previous: Option<(f32, f32, f32)> = None;
        for p in &lane.centreline {
            let x = quantise_f32(p.x, Q_POSITION_M);
            let y = quantise_f32(p.y, Q_POSITION_M);
            let z = quantise_f32(p.z, Q_HEIGHT_M);
            if previous == Some((x, y, z)) {
                return Err(WorldError::Unrepresentable {
                    what: format!("lane {} centreline", lane.id),
                    format: "vwp-world/1",
                    problem: format!(
                        "two successive points collapse to the same f32 at ({x}, {y}, {z}); \
                         §4.3 requires successive points to differ by at least 1 mm"
                    ),
                });
            }
            previous = Some((x, y, z));
            warnings.check("lane.x", p.x, || format!("lane {} centreline x", lane.id));
            warnings.check("lane.y", p.y, || format!("lane {} centreline y", lane.id));
            warnings.check("lane.z", p.z, || format!("lane {} centreline z", lane.id));
            put_f32(body, off_lane_points + 4 * point_cursor, x);
            put_f32(body, off_lane_points + 4 * t + 4 * point_cursor, y);
            put_f32(body, off_lane_points + 8 * t + 4 * point_cursor, z);
            point_cursor += 1;
        }
    }

    // §4.4 — buildings, struct-of-arrays, and the shared ring arrays.
    let b = building_count;
    let r = ring_point_total;
    let mut ring_cursor = 0usize;
    // Ring points narrow to `f32` exactly as lane points do, so they are warned about
    // exactly as lane points are: a world whose buildings reach past the limit while its
    // lanes do not is still a world that lost millimetres.
    let put_ring = |body: &mut [u8],
                    ring: &[Vec3],
                    ring_cursor: &mut usize,
                    warnings: &mut PrecisionWarnings,
                    kind: &'static str,
                    column_x: &'static str,
                    column_y: &'static str,
                    id: u32|
     -> usize {
        let start = *ring_cursor;
        for p in ring {
            warnings.check(column_x, p.x, || format!("{kind} {id} ring x"));
            warnings.check(column_y, p.y, || format!("{kind} {id} ring y"));
            put_f32(
                body,
                off_ring_points + 4 * *ring_cursor,
                quantise_f32(p.x, Q_POSITION_M),
            );
            put_f32(
                body,
                off_ring_points + 4 * r + 4 * *ring_cursor,
                quantise_f32(p.y, Q_POSITION_M),
            );
            *ring_cursor += 1;
        }
        start
    };
    for (i, building) in buildings.iter().enumerate() {
        let ring = building.open_ring();
        let start = put_ring(
            body,
            ring,
            &mut ring_cursor,
            &mut warnings,
            "building",
            "building.ring.x",
            "building.ring.y",
            building.id.index(),
        );
        warnings.check("building.base_z", building.base_z_m, || {
            format!("building {} base z", building.id)
        });
        put_u32(body, off_buildings + 4 * i, building.id.index());
        put_u32(body, off_buildings + 4 * b + 4 * i, start as u32);
        put_u32(body, off_buildings + 8 * b + 4 * i, ring.len() as u32);
        put_f32(
            body,
            off_buildings + 12 * b + 4 * i,
            quantise_f32(building.height_m, Q_HEIGHT_M),
        );
        put_f32(
            body,
            off_buildings + 16 * b + 4 * i,
            quantise_f32(building.base_z_m, Q_HEIGHT_M),
        );
        put_u32(
            body,
            off_buildings + 20 * b + 4 * i,
            building.name.map_or(0, |s| s.index()),
        );
        put_u8(
            body,
            off_buildings + 24 * b + i,
            building.material.wire_code(),
        );
        put_u8(body, off_buildings + 25 * b + i, building.lod.wire_code());
        put_u16(
            body,
            off_buildings + 26 * b + 2 * i,
            building.levels.unwrap_or(U16_NONE),
        );
    }

    // §4.5 — junctions.
    for (i, j) in junctions.iter().enumerate() {
        let at = off_junctions + JUNCTION_STRIDE * i;
        put_u32(body, at, j.id.index());
        put_u32(body, at + 4, j.name.map_or(0, |s| s.index()));
        warnings.check("junction.x", j.position.x, || {
            format!("junction {} x", j.id)
        });
        warnings.check("junction.y", j.position.y, || {
            format!("junction {} y", j.id)
        });
        warnings.check("junction.z", j.position.z, || {
            format!("junction {} z", j.id)
        });
        put_f32(body, at + 8, quantise_f32(j.position.x, Q_POSITION_M));
        put_f32(body, at + 12, quantise_f32(j.position.y, Q_POSITION_M));
        put_f32(body, at + 16, quantise_f32(j.position.z, Q_HEIGHT_M));
        put_u8(body, at + 20, j.control.wire_code());
        // §4.5's `lane_count` is a `u16` and §0 reserves `0xFFFF` for "absent", so the
        // largest number of lanes this column can *state* is 0xFFFE. Writing the sentinel
        // for an overflow — which `unwrap_or(U16_NONE)` did — would make "touches 65 535
        // lanes" indistinguishable from "not stated", so it is an error instead.
        let touching = j.incoming.len() + j.outgoing.len() + j.internal.len();
        put_u16(body, at + 22, lane_count_column(touching, j.id.index())?);
    }

    // §4.5 — signal heads.
    for (i, (plan, head)) in heads.iter().enumerate() {
        let at = off_signals + SIGNAL_STRIDE * i;
        put_u32(body, at, plan.id.index());
        put_u32(body, at + 4, plan.junction.index());
        put_u32(body, at + 8, head.lane.index());
        warnings.check("signal.x", head.position.x, || {
            format!("signal {} head x", plan.id)
        });
        warnings.check("signal.y", head.position.y, || {
            format!("signal {} head y", plan.id)
        });
        warnings.check("signal.z", head.position.z, || {
            format!("signal {} head z", plan.id)
        });
        put_f32(body, at + 12, quantise_f32(head.position.x, Q_POSITION_M));
        put_f32(body, at + 16, quantise_f32(head.position.y, Q_POSITION_M));
        put_f32(body, at + 20, quantise_f32(head.position.z, Q_HEIGHT_M));
        put_u8(body, at + 24, head.kind.wire_code());
        put_u16(body, at + 26, head.group);
    }

    // §4.5 — sites.
    for (i, s) in world.sites.iter().enumerate() {
        let at = off_sites + SITE_STRIDE * i;
        put_u32(body, at, s.id.index());
        put_u32(body, at + 4, s.node.map_or(U32_NONE, |n| n.index()));
        warnings.check("site.x", s.position.x, || format!("site {} x", s.id));
        warnings.check("site.y", s.position.y, || format!("site {} y", s.id));
        warnings.check("site.z", s.position.z, || format!("site {} z", s.id));
        warnings.check("site.antenna_height", s.antenna_height_m, || {
            format!("site {} antenna height", s.id)
        });
        put_f32(body, at + 8, quantise_f32(s.position.x, Q_POSITION_M));
        put_f32(body, at + 12, quantise_f32(s.position.y, Q_POSITION_M));
        put_f32(body, at + 16, quantise_f32(s.position.z, Q_HEIGHT_M));
        put_f32(body, at + 20, quantise_f32(s.antenna_height_m, Q_HEIGHT_M));
        put_f32(body, at + 24, quantise_f32(s.antenna_gain_dbi, Q_DB));
        put_u8(body, at + 28, s.kind.wire_code());
    }

    // §4.5 — crossings.
    for (i, c) in crossings.iter().enumerate() {
        let at = off_crossings + CROSSING_STRIDE * i;
        put_u32(body, at, c.id.index());
        put_u32(body, at + 4, c.junction.index());
        for (column, label, value) in [
            ("crossing.x1", "x1", c.from.x),
            ("crossing.y1", "y1", c.from.y),
            ("crossing.x2", "x2", c.to.x),
            ("crossing.y2", "y2", c.to.y),
        ] {
            warnings.check(column, value, || format!("crossing {} {label}", c.id));
        }
        put_f32(body, at + 8, quantise_f32(c.from.x, Q_POSITION_M));
        put_f32(body, at + 12, quantise_f32(c.from.y, Q_POSITION_M));
        put_f32(body, at + 16, quantise_f32(c.to.x, Q_POSITION_M));
        put_f32(body, at + 20, quantise_f32(c.to.y, Q_POSITION_M));
        put_f32(body, at + 24, quantise_f32(c.width_m, Q_POSITION_M));
    }

    // §4.5 — land use, whose rings live in the building ring arrays.
    for (i, z) in world.landuse.iter().enumerate() {
        let ring = z.open_ring();
        let start = put_ring(
            body,
            ring,
            &mut ring_cursor,
            &mut warnings,
            "landuse zone",
            "landuse.ring.x",
            "landuse.ring.y",
            z.id.index(),
        );
        let at = off_landuse + LANDUSE_STRIDE * i;
        put_u32(body, at, z.id.index());
        put_u32(body, at + 4, start as u32);
        put_u32(body, at + 8, ring.len() as u32);
        put_u8(body, at + 12, z.class.wire_code());
    }

    body[off_strings..off_strings + str_table.len()].copy_from_slice(&str_table);
    if off_provenance != 0 {
        body[off_provenance..off_provenance + provenance_json.len()]
            .copy_from_slice(&provenance_json);
    }

    // §4.2 — the content hash covers the body while its own field is still zero, which is
    // the only self-consistent reading of "SHA-256 of the body" for a field stored inside
    // the body, and is what the TypeScript client checks.
    let content_hash = v2xw_core::hash::sha256(&file[HEADER_BYTES..]);
    file[HEADER_BYTES + dir::CONTENT_HASH..HEADER_BYTES + dir::CONTENT_HASH + 32]
        .copy_from_slice(&content_hash);

    Ok(WorldPayload {
        bytes: file,
        content_hash,
        precision_warnings: warnings.out,
    })
}

/// Recomputes the content hash of a `vwp-world/1` file, the way a reader verifies it
/// (conformance W1, W3).
///
/// # Errors
///
/// [`WorldError::Malformed`] if the file is too short or has the wrong magic.
pub fn verify(file: &[u8]) -> Result<[u8; 32]> {
    if file.len() < HEADER_BYTES + DIRECTORY_BYTES {
        return Err(WorldError::Malformed {
            offset: 0,
            problem: format!(
                "{} bytes is shorter than a header plus a directory",
                file.len()
            ),
        });
    }
    let magic = u32::from_le_bytes([file[0], file[1], file[2], file[3]]);
    if magic != MAGIC {
        return Err(WorldError::Malformed {
            offset: 0,
            problem: format!("bad magic {magic:#010x}, expected {MAGIC:#010x} (\"VWLD\")"),
        });
    }
    let mut body = file[HEADER_BYTES..].to_vec();
    body[dir::CONTENT_HASH..dir::CONTENT_HASH + 32].fill(0);
    Ok(v2xw_core::hash::sha256(&body))
}

/// The stored content hash of a `vwp-world/1` file.
///
/// # Errors
///
/// [`WorldError::Malformed`] if the file is too short.
pub fn stored_content_hash(file: &[u8]) -> Result<[u8; 32]> {
    if file.len() < HEADER_BYTES + 32 {
        return Err(WorldError::Malformed {
            offset: 0,
            problem: "file is too short to hold a content hash".to_string(),
        });
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&file[HEADER_BYTES..HEADER_BYTES + 32]);
    Ok(out)
}

// ---------------------------------------------------------------------------
// §4.6 — the JSON mirror form
// ---------------------------------------------------------------------------

/// The `vwp-world/1` JSON form (§4.6): the same content as the binary, for debugging,
/// tests and third-party tools.
///
/// It is a direct transcription — each binary section becomes an array of objects, and
/// the parallel `f32` point arrays become flat number arrays — and it carries the
/// **binary** form's content hash, as §4.6 requires ("the hash is of the binary body; the
/// JSON carries it for cross-checking"). Every number is the `f32`-narrowed value the
/// binary holds, so conformance item W4 (`world_json_binary_parity`) compares equal
/// values rather than nearly-equal ones.
///
/// # Errors
///
/// Whatever [`write()`] rejects: the JSON form needs the binary form's hash, so it builds
/// it.
pub fn to_json(world: &World) -> Result<serde_json::Value> {
    let payload = write(world)?;
    to_json_with_hash(world, payload.content_hash)
}

/// The JSON form with a content hash supplied by the caller, for a server that already
/// wrote the binary and does not want to write it twice.
///
/// # Errors
///
/// [`WorldError::Json`] if the provenance will not serialise.
pub fn to_json_with_hash(world: &World, content_hash: [u8; 32]) -> Result<serde_json::Value> {
    // One `f32`-narrowed number, exactly as the binary form stores it, so §4.6's "direct
    // transcription" holds value for value.
    //
    // `serde_json` has no way to write a non-finite number and turns one into `null`,
    // which would *not* be a transcription of the binary form — where `NaN` is §0's
    // "absent" sentinel and is stored as itself. That divergence is closed upstream
    // rather than here: [`crate::model::World::validate`] refuses a world carrying a
    // non-finite float at all ([`crate::WorldError::NonFinite`]), so no `World` that
    // exists can reach this closure with one.
    let f = |v: f64, q: f64| -> serde_json::Value { f64::from(quantise_f32(v, q)).into() };

    let lanes: Vec<serde_json::Value> = world
        .roads
        .lanes()
        .iter()
        .map(|lane| {
            let mut centreline = Vec::with_capacity(lane.point_count() * 3);
            for p in &lane.centreline {
                centreline.push(f(p.x, Q_POSITION_M));
                centreline.push(f(p.y, Q_POSITION_M));
                centreline.push(f(p.z, Q_HEIGHT_M));
            }
            serde_json::json!({
                "lane_id": lane.id.index(),
                "edge_id": lane.edge.index(),
                "junction_id": lane.junction.map(|j| j.index()),
                "name": world.symbols.resolve(crate::model::SymbolId::new(lane_name_symbol(world, lane))),
                "width_m": f(lane.width_m, Q_POSITION_M),
                "speed_limit_mps": f(lane.speed_limit_mps, Q_SPEED_MPS),
                "lane_type": lane.kind.wire_name(),
                "index_in_edge": lane.index,
                "allowed_classes": lane.allowed.names(),
                "centreline": centreline,
            })
        })
        .collect();

    let ring_json = |ring: &[Vec3]| -> Vec<serde_json::Value> {
        let mut out = Vec::with_capacity(ring.len() * 2);
        for p in ring {
            out.push(f(p.x, Q_POSITION_M));
            out.push(f(p.y, Q_POSITION_M));
        }
        out
    };

    let buildings: Vec<serde_json::Value> = world
        .buildings
        .iter()
        .map(|b: &Building| {
            serde_json::json!({
                "building_id": b.id.index(),
                "height_m": f(b.height_m, Q_HEIGHT_M),
                "base_z_m": f(b.base_z_m, Q_HEIGHT_M),
                "levels": b.levels,
                "material": b.material.wire_name(),
                "lod_hint": b.lod.wire_name(),
                "name": world.symbols.resolve_optional(b.name),
                "ring": ring_json(b.open_ring()),
            })
        })
        .collect();

    let junctions: Vec<serde_json::Value> = world
        .roads
        .junctions()
        .iter()
        .map(|j| {
            serde_json::json!({
                "junction_id": j.id.index(),
                "name": world.symbols.resolve_optional(j.name),
                "x_m": f(j.position.x, Q_POSITION_M),
                "y_m": f(j.position.y, Q_POSITION_M),
                "z_m": f(j.position.z, Q_HEIGHT_M),
                "control": j.control.wire_name(),
                "lane_count": j.incoming.len() + j.outgoing.len() + j.internal.len(),
            })
        })
        .collect();

    let signals: Vec<serde_json::Value> = heads_of(world)
        .into_iter()
        .map(|(plan, head)| {
            serde_json::json!({
                "signal_id": plan.id.index(),
                "junction_id": plan.junction.index(),
                "lane_id": head.lane.index(),
                "x_m": f(head.position.x, Q_POSITION_M),
                "y_m": f(head.position.y, Q_POSITION_M),
                "z_m": f(head.position.z, Q_HEIGHT_M),
                "kind": head.kind.wire_name(),
                "group": head.group,
            })
        })
        .collect();

    let sites: Vec<serde_json::Value> = world
        .sites
        .iter()
        .map(|s| {
            serde_json::json!({
                "site_id": s.id.index(),
                "node_id": s.node.map(|n| n.index()),
                "x_m": f(s.position.x, Q_POSITION_M),
                "y_m": f(s.position.y, Q_POSITION_M),
                "z_m": f(s.position.z, Q_HEIGHT_M),
                "antenna_height_m": f(s.antenna_height_m, Q_HEIGHT_M),
                "antenna_gain_dbi": f(s.antenna_gain_dbi, Q_DB),
                "kind": s.kind.wire_name(),
            })
        })
        .collect();

    let crossings: Vec<serde_json::Value> = world
        .roads
        .crossings()
        .iter()
        .map(|c| {
            serde_json::json!({
                "crossing_id": c.id.index(),
                "junction_id": c.junction.index(),
                "x1_m": f(c.from.x, Q_POSITION_M),
                "y1_m": f(c.from.y, Q_POSITION_M),
                "x2_m": f(c.to.x, Q_POSITION_M),
                "y2_m": f(c.to.y, Q_POSITION_M),
                "width_m": f(c.width_m, Q_POSITION_M),
            })
        })
        .collect();

    let landuse: Vec<serde_json::Value> = world
        .landuse
        .iter()
        .map(|z| {
            serde_json::json!({
                "landuse_id": z.id.index(),
                "class": z.class.wire_name(),
                "ring": ring_json(z.open_ring()),
            })
        })
        .collect();

    Ok(serde_json::json!({
        "schema": "vwp-world/1",
        "content_hash": v2xw_core::hash::hex_encode(&content_hash),
        "origin": {
            "lat_deg": world.origin.lat_deg,
            "lon_deg": world.origin.lon_deg,
            "alt_m": world.origin.alt_m,
        },
        "bbox": {
            "min_x_m": world.bbox.min.x,
            "min_y_m": world.bbox.min.y,
            "max_x_m": world.bbox.max.x,
            "max_y_m": world.bbox.max.y,
            "min_z_m": f(world.bbox.min.z, Q_HEIGHT_M),
            "max_z_m": f(world.bbox.max.z, Q_HEIGHT_M),
        },
        "lanes": lanes,
        "buildings": buildings,
        "junctions": junctions,
        "signals": signals,
        "sites": sites,
        "crossings": crossings,
        "landuse": landuse,
        "provenance": world.provenance.to_wire_json(),
    }))
}

/// The JSON form as a string, the body of `GET /world/{hash}.json`.
///
/// # Errors
///
/// Whatever [`to_json`] rejects.
pub fn to_json_string(world: &World) -> Result<String> {
    Ok(serde_json::to_string(&to_json(world)?)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn str_table_matches_the_specification() {
        // §2.5: n, blob_bytes, n + 1 offsets, then the blob padded to a multiple of four.
        let table = encode_str_table(&["".to_string(), "abc".to_string(), "de".to_string()]);
        assert_eq!(table.len(), 8 + 4 * 4 + 8, "5 bytes of blob, padded to 8");
        assert_eq!(&table[0..4], &3u32.to_le_bytes());
        assert_eq!(&table[4..8], &5u32.to_le_bytes());
        assert_eq!(&table[8..12], &0u32.to_le_bytes());
        assert_eq!(&table[12..16], &0u32.to_le_bytes());
        assert_eq!(&table[16..20], &3u32.to_le_bytes());
        assert_eq!(&table[20..24], &5u32.to_le_bytes());
        assert_eq!(&table[24..29], b"abcde");
        assert_eq!(&table[29..], &[0, 0, 0], "zero padding");
    }

    #[test]
    fn empty_str_table_is_still_well_formed() {
        let table = encode_str_table(&["".to_string()]);
        assert_eq!(table.len(), 8 + 8);
        assert_eq!(&table[0..4], &1u32.to_le_bytes());
        assert_eq!(&table[4..8], &0u32.to_le_bytes());
    }

    #[test]
    fn ceil4_rounds_up() {
        assert_eq!(ceil4(0), 0);
        assert_eq!(ceil4(1), 4);
        assert_eq!(ceil4(4), 4);
        assert_eq!(ceil4(5), 8);
    }

    #[test]
    fn verify_rejects_a_file_that_is_not_one() {
        assert!(verify(b"").is_err());
        assert!(verify(&[0u8; 300]).is_err());
        assert!(stored_content_hash(b"short").is_err());
    }

    /// R15(1): a junction `lane_count` that does not fit is an error, not §0's "absent"
    /// sentinel.
    ///
    /// The guard is tested here rather than through [`write`] because reaching it needs a
    /// junction that touches 65 535 *existing* lanes — `World::validate` rejects a
    /// junction that lists a lane id it does not have — and building 65 535 lanes to test
    /// one comparison is a minute of CI for nothing. The decision is the fix, and this is
    /// the decision.
    #[test]
    fn a_lane_count_that_cannot_be_stated_is_an_error_not_the_absent_sentinel() {
        assert_eq!(lane_count_column(0, 7).unwrap(), 0);
        assert_eq!(lane_count_column(12, 7).unwrap(), 12);
        assert_eq!(lane_count_column(65_534, 7).unwrap(), 0xFFFE);
        // What the writer used to do with a count it could not represent: write the value
        // §0 reserves for "absent", making an overflow indistinguishable from "not
        // stated".
        assert_eq!(u16::try_from(65_535usize).unwrap_or(U16_NONE), U16_NONE);
        assert_eq!(u16::try_from(70_000usize).unwrap_or(U16_NONE), U16_NONE);
        // What it does now.
        for touching in [65_535usize, 70_000, usize::from(u16::MAX) + 1] {
            let err = lane_count_column(touching, 7).expect_err("must not be writable");
            assert!(
                matches!(err, WorldError::Unrepresentable { .. }),
                "{touching} lanes gave {err:?}"
            );
        }
    }

    /// R15(2): a size, offset or count that does not fit a `u32` is an error, not a
    /// wrapped cast that writes a structurally corrupt file.
    #[test]
    fn a_u32_field_that_would_wrap_is_an_error() {
        assert_eq!(u32_field("body_len", 0).unwrap(), 0);
        assert_eq!(
            u32_field("body_len", u32::MAX as usize).unwrap(),
            u32::MAX,
            "the largest describable payload is still describable"
        );
        // What an `as u32` cast does with 4 GiB + 1: it writes zero.
        assert_eq!((u32::MAX as usize + 1) as u32, 0);
        let err = u32_field("body_len", u32::MAX as usize + 1).expect_err("must not fit");
        assert!(matches!(err, WorldError::Unrepresentable { .. }), "{err:?}");
    }

    /// R14: the precision warning covers every narrowed column, and says each one once.
    #[test]
    fn precision_warnings_are_one_per_column() {
        let mut w = PrecisionWarnings::default();
        let far = F32_MM_GRID_LIMIT_M + 1.0;
        w.check("lane.x", far, || "lane 1 centreline x".to_string());
        w.check("lane.x", far + 1.0, || "lane 2 centreline x".to_string());
        w.check("building.ring.x", far, || "building 3 ring x".to_string());
        w.check("junction.x", far, || "junction 4 x".to_string());
        // Inside the limit: no warning, whatever the column.
        w.check("site.x", F32_MM_GRID_LIMIT_M - 1.0, || {
            "site 5 x".to_string()
        });
        assert_eq!(w.out.len(), 3, "{:#?}", w.out);
        assert!(w.out[0].starts_with("lane 1 centreline x"));
        assert!(w.out[1].starts_with("building 3 ring x"));
        assert!(w.out[2].starts_with("junction 4 x"));
        assert!(w.out.iter().all(|m| m.contains("16384")));
    }
}
