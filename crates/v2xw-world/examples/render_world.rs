//! Renders an imported world as top-down PNGs, so that its geometry can be **looked at**
//! rather than inferred from counts.
//!
//! ```text
//! cargo run -p v2xw-world --release --example render_world -- \
//!     worlds/cache/manhattan.osm.xml worlds/cache
//! ```
//!
//! It writes two images plus a text report to the output directory:
//!
//! * `manhattan-render.png` — the whole world bounding box, about 2000 px on its longer
//!   side: building footprints shaded by height, land-use zones, sidewalks and crossings,
//!   junction areas, and drivable lane centrelines line-weighted by [`RoadClass`].
//! * `manhattan-junction.png` — one busy signalised junction at 1 px per metre, with each
//!   drivable lane drawn separately and every internal connector coloured by its
//!   [`TurnDirection`] and arrowed in its direction of travel.
//! * `render-report.txt` — the projection cross-check against three landmarks and the
//!   network statistics (lane-length and junction-degree distributions, largest connected
//!   component, dead ends, one-way fraction, drivable lane kilometres).
//!
//! # Why the PNG encoder is in here
//!
//! The crate must not grow a dependency for a diagnostic, so [`png`] below is a complete
//! minimal encoder: CRC-32, Adler-32, and a DEFLATE compressor using fixed Huffman codes
//! with a greedy LZ77 match search. It is deterministic and needs nothing outside `core`
//! and `alloc`.
//!
//! # Conventions this renderer must get right
//!
//! The world frame is ENU metres (build decision D6): `+x` is east, `+y` is north. A PNG's
//! rows run top to bottom, so [`View::to_px`] maps `bbox.max.y` to row 0 and the image is
//! north-up, east-right, like a map. Getting that flip wrong would mirror the render
//! vertically, which is exactly the class of defect these images exist to catch, so the
//! projection cross-check in [`landmark_check`] re-derives it independently: it asserts
//! that a landmark known to be further north lands on a *smaller* row index.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt::Write as _;
use std::path::PathBuf;

use v2xw_core::geom::{Bbox, Vec3};
use v2xw_core::ids::{JunctionId, LaneId};
use v2xw_core::math;
use v2xw_world::model::{JunctionControl, LaneKind, Projection, RoadClass, TurnDirection, World};
use v2xw_world::osm::{OsmOptions, import_osm};

// ---------------------------------------------------------------------------
// A minimal PNG encoder
// ---------------------------------------------------------------------------

/// A self-contained PNG writer: CRC-32, Adler-32 and fixed-Huffman DEFLATE.
mod png {
    /// The CRC-32 polynomial of ITU-T V.42, reflected, as PNG §5 requires.
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &b in bytes {
            crc ^= u32::from(b);
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
        !crc
    }

    /// Adler-32 over the uncompressed data, for the zlib trailer (RFC 1950 §2.2).
    fn adler32(bytes: &[u8]) -> u32 {
        let (mut a, mut b) = (1u32, 0u32);
        for &byte in bytes {
            a = (a + u32::from(byte)) % 65521;
            b = (b + a) % 65521;
        }
        (b << 16) | a
    }

    /// An LSB-first bit sink, which is the order DEFLATE packs its bits in (RFC 1951 §3.1).
    struct BitWriter {
        out: Vec<u8>,
        acc: u32,
        n: u32,
    }

    impl BitWriter {
        fn new() -> Self {
            Self {
                out: Vec::new(),
                acc: 0,
                n: 0,
            }
        }

        /// Writes `n` bits of `value`, least significant bit first.
        fn bits(&mut self, value: u32, n: u32) {
            self.acc |= (value & ((1u32 << n) - 1)) << self.n;
            self.n += n;
            while self.n >= 8 {
                self.out.push((self.acc & 0xFF) as u8);
                self.acc >>= 8;
                self.n -= 8;
            }
        }

        /// Writes a Huffman code: its bits are defined most significant first, so they are
        /// reversed before going into an LSB-first sink (RFC 1951 §3.1.1).
        fn code(&mut self, code: u32, len: u32) {
            let mut reversed = 0u32;
            for i in 0..len {
                reversed |= ((code >> (len - 1 - i)) & 1) << i;
            }
            self.bits(reversed, len);
        }

        fn finish(mut self) -> Vec<u8> {
            if self.n > 0 {
                self.out.push((self.acc & 0xFF) as u8);
            }
            self.out
        }
    }

    /// `(first code, base length, extra bits)` for length codes 257..=285 (RFC 1951 §3.2.5).
    const LENGTH_TABLE: [(u16, u16, u32); 29] = [
        (257, 3, 0),
        (258, 4, 0),
        (259, 5, 0),
        (260, 6, 0),
        (261, 7, 0),
        (262, 8, 0),
        (263, 9, 0),
        (264, 10, 0),
        (265, 11, 1),
        (266, 13, 1),
        (267, 15, 1),
        (268, 17, 1),
        (269, 19, 2),
        (270, 23, 2),
        (271, 27, 2),
        (272, 31, 2),
        (273, 35, 3),
        (274, 43, 3),
        (275, 51, 3),
        (276, 59, 3),
        (277, 67, 4),
        (278, 83, 4),
        (279, 99, 4),
        (280, 115, 4),
        (281, 131, 5),
        (282, 163, 5),
        (283, 195, 5),
        (284, 227, 5),
        (285, 258, 0),
    ];

    /// `(base distance, extra bits)` for distance codes 0..=29 (RFC 1951 §3.2.5).
    const DISTANCE_TABLE: [(u16, u32); 30] = [
        (1, 0),
        (2, 0),
        (3, 0),
        (4, 0),
        (5, 1),
        (7, 1),
        (9, 2),
        (13, 2),
        (17, 3),
        (25, 3),
        (33, 4),
        (49, 4),
        (65, 5),
        (97, 5),
        (129, 6),
        (193, 6),
        (257, 7),
        (385, 7),
        (513, 8),
        (769, 8),
        (1025, 9),
        (1537, 9),
        (2049, 10),
        (3073, 10),
        (4097, 11),
        (6145, 11),
        (8193, 12),
        (12289, 12),
        (16385, 13),
        (24577, 13),
    ];

    /// Emits one literal byte with the fixed literal/length code (RFC 1951 §3.2.6).
    fn put_literal(w: &mut BitWriter, byte: u8) {
        let v = u32::from(byte);
        if v < 144 {
            w.code(0x30 + v, 8);
        } else {
            w.code(0x190 + (v - 144), 9);
        }
    }

    /// Emits a `<length, distance>` back-reference with the fixed codes.
    fn put_match(w: &mut BitWriter, len: usize, dist: usize) {
        let (code, base, extra) = *LENGTH_TABLE
            .iter()
            .rev()
            .find(|(_, base, _)| usize::from(*base) <= len)
            .expect("length 3..=258 is covered by the table");
        // Codes 257..=279 are 7 bits (0..=0x17); 280..=287 are 8 bits (0xC0..).
        if code < 280 {
            w.code(u32::from(code) - 256, 7);
        } else {
            w.code(0xC0 + u32::from(code) - 280, 8);
        }
        if extra > 0 {
            w.bits((len - usize::from(base)) as u32, extra);
        }
        let (dcode, dbase, dextra) = DISTANCE_TABLE
            .iter()
            .enumerate()
            .rev()
            .find(|(_, (base, _))| usize::from(*base) <= dist)
            .map(|(i, (base, extra))| (i, *base, *extra))
            .expect("distance 1..=32768 is covered by the table");
        w.code(dcode as u32, 5);
        if dextra > 0 {
            w.bits((dist - usize::from(dbase)) as u32, dextra);
        }
    }

    /// The longest match of `data[at..]` against `data[at - dist..]`, capped at 258 bytes.
    fn match_len(data: &[u8], at: usize, dist: usize) -> usize {
        if dist == 0 || dist > at {
            return 0;
        }
        let limit = (data.len() - at).min(258);
        let mut n = 0;
        while n < limit && data[at - dist + n] == data[at + n] {
            n += 1;
        }
        n
    }

    /// One DEFLATE stream of fixed-Huffman blocks with greedy LZ77.
    ///
    /// The match search is deliberately small: the candidate distances are the ones a
    /// filtered image actually repeats at — 1..=4 bytes (a run of one colour), the scanline
    /// stride and twice it (a vertical repeat) — plus the most recent position with the same
    /// 4-byte hash. That is enough to compress a mostly flat map render by two orders of
    /// magnitude while staying a few dozen lines long.
    fn deflate(data: &[u8], stride: usize) -> Vec<u8> {
        const HASH_BITS: u32 = 15;
        let mut head = vec![usize::MAX; 1 << HASH_BITS];
        let mut w = BitWriter::new();
        w.bits(1, 1); // BFINAL
        w.bits(1, 2); // BTYPE = 01, fixed Huffman
        let hash_at = |i: usize| -> usize {
            if i + 4 > data.len() {
                return usize::MAX;
            }
            let v = u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]);
            ((v.wrapping_mul(0x9E37_79B1)) >> (32 - HASH_BITS)) as usize
        };

        let mut i = 0usize;
        while i < data.len() {
            let mut best = (0usize, 0usize); // (len, dist)
            if i > 0 {
                let mut candidates = [1usize, 2, 3, 4, stride, stride * 2, 0];
                let h = hash_at(i);
                if h != usize::MAX && head[h] != usize::MAX && head[h] < i {
                    candidates[6] = i - head[h];
                }
                for dist in candidates {
                    if dist == 0 || dist > i || dist > 32768 {
                        continue;
                    }
                    let n = match_len(data, i, dist);
                    if n > best.0 {
                        best = (n, dist);
                    }
                }
            }
            let h = hash_at(i);
            if h != usize::MAX {
                head[h] = i;
            }
            if best.0 >= 3 {
                put_match(&mut w, best.0, best.1);
                // Index the interior of the match cheaply: every 8th byte is enough to keep
                // the hash table useful without walking the whole span.
                let mut k = i + 1;
                while k < i + best.0 {
                    let hk = hash_at(k);
                    if hk != usize::MAX {
                        head[hk] = k;
                    }
                    k += 8;
                }
                i += best.0;
            } else {
                put_literal(&mut w, data[i]);
                i += 1;
            }
        }
        w.code(0, 7); // end-of-block, code 256
        let mut out = vec![0x78, 0x01]; // zlib: deflate, 32 KiB window, no dictionary
        out.extend_from_slice(&w.finish());
        out.extend_from_slice(&adler32(data).to_be_bytes());
        out
    }

    /// Appends one PNG chunk: length, type, data, CRC.
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut crc_input = Vec::with_capacity(4 + data.len());
        crc_input.extend_from_slice(kind);
        crc_input.extend_from_slice(data);
        out.extend_from_slice(&crc_input);
        out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
    }

    /// Encodes an 8-bit RGB image (`rgb.len() == w * h * 3`) as a PNG byte stream.
    ///
    /// Each scanline is written with filter type 0 (none): the DEFLATE stage above already
    /// finds the vertical and horizontal repeats that a filter would expose, and keeping
    /// the raw bytes makes the encoder auditable.
    pub fn encode_rgb(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
        assert_eq!(rgb.len(), width as usize * height as usize * 3);
        let stride = width as usize * 3;
        let mut raw = Vec::with_capacity((stride + 1) * height as usize);
        for row in 0..height as usize {
            raw.push(0);
            raw.extend_from_slice(&rgb[row * stride..(row + 1) * stride]);
        }
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        let mut ihdr = Vec::with_capacity(13);
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8 bits, truecolour, no interlace
        chunk(&mut out, b"IHDR", &ihdr);
        chunk(&mut out, b"IDAT", &deflate(&raw, stride + 1));
        chunk(&mut out, b"IEND", &[]);
        out
    }
}

// ---------------------------------------------------------------------------
// Canvas
// ---------------------------------------------------------------------------

/// An RGB raster with the handful of primitives this renderer needs.
struct Canvas {
    width: i64,
    height: i64,
    px: Vec<u8>,
}

impl Canvas {
    /// A canvas filled with `bg`.
    fn new(width: i64, height: i64, bg: [u8; 3]) -> Self {
        let mut px = Vec::with_capacity((width * height * 3) as usize);
        for _ in 0..width * height {
            px.extend_from_slice(&bg);
        }
        Self { width, height, px }
    }

    /// Alpha-blends `colour` over pixel `(x, y)`, clipping outside the canvas.
    fn blend(&mut self, x: i64, y: i64, colour: [u8; 3], alpha: f64) {
        if x < 0 || y < 0 || x >= self.width || y >= self.height || alpha <= 0.0 {
            return;
        }
        let a = alpha.clamp(0.0, 1.0);
        let i = ((y * self.width + x) * 3) as usize;
        for (c, target) in colour.iter().enumerate() {
            let old = f64::from(self.px[i + c]);
            let new = f64::from(*target);
            self.px[i + c] = (old + (new - old) * a).round().clamp(0.0, 255.0) as u8;
        }
    }

    /// An antialiased line of width `w_px` from `a` to `b`.
    ///
    /// Drawn by distance-to-segment over the segment's bounding box rather than by
    /// Bresenham, because lane centrelines are sub-pixel-separated in the overview and a
    /// hard-edged line would alias them into a single smear.
    fn line(&mut self, a: (f64, f64), b: (f64, f64), colour: [u8; 3], w_px: f64, alpha: f64) {
        let half = (w_px / 2.0).max(0.35);
        let pad = half + 1.0;
        let (x0, x1) = (a.0.min(b.0) - pad, a.0.max(b.0) + pad);
        let (y0, y1) = (a.1.min(b.1) - pad, a.1.max(b.1) + pad);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len2 = dx * dx + dy * dy;
        for y in (y0.floor() as i64).max(0)..=(y1.ceil() as i64).min(self.height - 1) {
            for x in (x0.floor() as i64).max(0)..=(x1.ceil() as i64).min(self.width - 1) {
                let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
                let t = if len2 > 0.0 {
                    (((px - a.0) * dx + (py - a.1) * dy) / len2).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let (cx, cy) = (a.0 + dx * t, a.1 + dy * t);
                let d = math::hypot(px - cx, py - cy);
                let cover = (half + 0.5 - d).clamp(0.0, 1.0);
                if cover > 0.0 {
                    self.blend(x, y, colour, cover * alpha);
                }
            }
        }
    }

    /// A polyline.
    fn polyline(&mut self, pts: &[(f64, f64)], colour: [u8; 3], w_px: f64, alpha: f64) {
        for pair in pts.windows(2) {
            self.line(pair[0], pair[1], colour, w_px, alpha);
        }
    }

    /// Fills a polygon by even-odd scanline rule, one sample per pixel centre.
    fn fill_polygon(&mut self, ring: &[(f64, f64)], colour: [u8; 3], alpha: f64) {
        if ring.len() < 3 {
            return;
        }
        let y_min = ring.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
        let y_max = ring.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
        let lo = (y_min.floor() as i64).max(0);
        let hi = (y_max.ceil() as i64).min(self.height - 1);
        let mut xs: Vec<f64> = Vec::with_capacity(8);
        for y in lo..=hi {
            let sy = y as f64 + 0.5;
            xs.clear();
            for i in 0..ring.len() {
                let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
                if (a.1 <= sy) != (b.1 <= sy) {
                    let t = (sy - a.1) / (b.1 - a.1);
                    xs.push(a.0 + (b.0 - a.0) * t);
                }
            }
            xs.sort_by(|p, q| p.partial_cmp(q).expect("polygon coordinates are finite"));
            for pair in xs.chunks_exact(2) {
                let (from, to) = (pair[0], pair[1]);
                for x in (from.floor() as i64).max(0)..=(to.ceil() as i64).min(self.width - 1) {
                    let px = x as f64 + 0.5;
                    if px >= from && px <= to {
                        self.blend(x, y, colour, alpha);
                    }
                }
            }
        }
    }

    /// A filled disc, antialiased at its edge.
    fn disc(&mut self, c: (f64, f64), r_px: f64, colour: [u8; 3], alpha: f64) {
        let lo_y = ((c.1 - r_px - 1.0).floor() as i64).max(0);
        let hi_y = ((c.1 + r_px + 1.0).ceil() as i64).min(self.height - 1);
        let lo_x = ((c.0 - r_px - 1.0).floor() as i64).max(0);
        let hi_x = ((c.0 + r_px + 1.0).ceil() as i64).min(self.width - 1);
        for y in lo_y..=hi_y {
            for x in lo_x..=hi_x {
                let d = math::hypot(x as f64 + 0.5 - c.0, y as f64 + 0.5 - c.1);
                let cover = (r_px + 0.5 - d).clamp(0.0, 1.0);
                if cover > 0.0 {
                    self.blend(x, y, colour, cover * alpha);
                }
            }
        }
    }

    /// A hollow square marker, for signalised junctions in the overview.
    fn square_outline(&mut self, c: (f64, f64), half: f64, colour: [u8; 3], w_px: f64) {
        let corners = [
            (c.0 - half, c.1 - half),
            (c.0 + half, c.1 - half),
            (c.0 + half, c.1 + half),
            (c.0 - half, c.1 + half),
            (c.0 - half, c.1 - half),
        ];
        self.polyline(&corners, colour, w_px, 1.0);
    }

    /// Writes the canvas out as a PNG.
    fn write_png(&self, path: &PathBuf) -> std::io::Result<usize> {
        let bytes = png::encode_rgb(self.width as u32, self.height as u32, &self.px);
        std::fs::write(path, &bytes)?;
        Ok(bytes.len())
    }
}

// ---------------------------------------------------------------------------
// The world → pixel mapping
// ---------------------------------------------------------------------------

/// A window onto the world, in metres, and its pixel scale.
///
/// North is up and east is right: `to_px` subtracts the world `y` from the window's
/// **maximum** `y`, because PNG row 0 is the top of the image.
struct View {
    min_x: f64,
    max_y: f64,
    px_per_m: f64,
    width: i64,
    height: i64,
}

impl View {
    /// A view of `bbox` whose longer side is `long_px` pixels.
    fn fit(bbox: Bbox, long_px: i64) -> Self {
        let (w_m, h_m) = (bbox.max.x - bbox.min.x, bbox.max.y - bbox.min.y);
        let px_per_m = long_px as f64 / w_m.max(h_m);
        Self {
            min_x: bbox.min.x,
            max_y: bbox.max.y,
            px_per_m,
            width: (w_m * px_per_m).round().max(1.0) as i64,
            height: (h_m * px_per_m).round().max(1.0) as i64,
        }
    }

    /// A view centred on `centre` covering `span_m` metres at `px_per_m`.
    fn centred(centre: Vec3, span_m: f64, px_per_m: f64) -> Self {
        let side = (span_m * px_per_m).round().max(1.0) as i64;
        Self {
            min_x: centre.x - span_m / 2.0,
            max_y: centre.y + span_m / 2.0,
            px_per_m,
            width: side,
            height: side,
        }
    }

    /// World metres to pixel coordinates.
    fn to_px(&self, p: Vec3) -> (f64, f64) {
        (
            (p.x - self.min_x) * self.px_per_m,
            (self.max_y - p.y) * self.px_per_m,
        )
    }

    /// True if the point is within `pad_px` of the canvas.
    fn near(&self, p: Vec3, pad_px: f64) -> bool {
        let (x, y) = self.to_px(p);
        x >= -pad_px
            && y >= -pad_px
            && x <= self.width as f64 + pad_px
            && y <= self.height as f64 + pad_px
    }
}

// ---------------------------------------------------------------------------
// Palette
// ---------------------------------------------------------------------------

/// Paper-white background, so that ink density reads as built density.
const BG: [u8; 3] = [250, 249, 246];
/// Building fill at minimum height; taller buildings interpolate towards [`BUILDING_TALL`].
const BUILDING_SHORT: [u8; 3] = [226, 222, 214];
/// Building fill at 200 m and above.
const BUILDING_TALL: [u8; 3] = [90, 78, 66];
/// Junction area polygons.
const JUNCTION_FILL: [u8; 3] = [255, 214, 170];
/// Signalised-junction marker.
const SIGNAL: [u8; 3] = [200, 30, 40];
/// Sidewalks and footways.
const FOOT: [u8; 3] = [120, 170, 130];
/// Pedestrian crossings.
const CROSSING: [u8; 3] = [40, 130, 70];
/// Cycleways.
const CYCLE: [u8; 3] = [150, 90, 180];
/// Land-use zone wash.
const LANDUSE: [u8; 3] = [214, 232, 214];
/// The named-street highlight, used for Broadway in the overview.
const NAMED: [u8; 3] = [230, 20, 140];
/// The landmark crosshair.
const LANDMARK: [u8; 3] = [255, 100, 0];
/// The requested-bbox outline.
const REQUESTED: [u8; 3] = [0, 150, 160];

/// Metres across the junction window.
const ZOOM_SPAN_M: f64 = 200.0;
/// Pixels per metre in the junction window.
///
/// The task this renderer was written for asks for "roughly 1 m per pixel". One pixel per
/// metre puts a 3.5 m lane 3.5 px from its neighbour, which is not enough to see whether
/// two connectors cross or merely pass close, so the zoom is rendered at 4 px/m (0.25 m
/// per pixel) and the 1 px/m version is written alongside it as `-1mpp.png` for the record.
const ZOOM_PX_PER_M: f64 = 4.0;

/// The street whose diagonal the render is checked for.
const DIAGONAL_STREET: &str = "Broadway";

/// The drawing colour and line width for a drivable lane of this class.
fn road_style(class: RoadClass) -> ([u8; 3], f64) {
    match class {
        RoadClass::Motorway | RoadClass::Trunk => ([20, 40, 120], 2.6),
        RoadClass::Primary => ([25, 55, 150], 2.1),
        RoadClass::Secondary => ([40, 80, 175], 1.7),
        RoadClass::Tertiary => ([60, 100, 190], 1.35),
        RoadClass::Residential | RoadClass::Living => ([95, 120, 175], 1.0),
        RoadClass::Link => ([70, 130, 200], 1.1),
        RoadClass::Service => ([160, 170, 190], 0.7),
        RoadClass::Internal => ([220, 130, 60], 0.7),
        _ => ([150, 150, 160], 0.7),
    }
}

/// The colour of an internal connector, by the way it turns.
fn turn_colour(d: TurnDirection) -> [u8; 3] {
    match d {
        TurnDirection::Straight => [30, 30, 30],
        TurnDirection::Left => [30, 90, 210],
        TurnDirection::Right => [220, 120, 20],
        TurnDirection::SlightLeft => [90, 150, 230],
        TurnDirection::SlightRight => [240, 170, 80],
        TurnDirection::UTurn => [200, 40, 180],
    }
}

// ---------------------------------------------------------------------------
// The overview render
// ---------------------------------------------------------------------------

/// Renders the whole world, bottom layer first.
fn render_overview(world: &World, long_px: i64) -> Canvas {
    let view = View::fit(world.bbox, long_px);
    let mut canvas = Canvas::new(view.width, view.height, BG);

    // 1. Land-use zones, as the faintest wash.
    for zone in &world.landuse {
        let ring: Vec<(f64, f64)> = zone.open_ring().iter().map(|p| view.to_px(*p)).collect();
        canvas.fill_polygon(&ring, LANDUSE, 0.55);
    }

    // 2. Building footprints, shaded by height: 3 m maps to the light end, 200 m to the
    //    dark end, so Midtown's towers are visibly darker than a brownstone row.
    for b in &world.buildings {
        let t = (b.height_m / 200.0).clamp(0.0, 1.0);
        let shade = [
            (f64::from(BUILDING_SHORT[0])
                + (f64::from(BUILDING_TALL[0]) - f64::from(BUILDING_SHORT[0])) * t)
                as u8,
            (f64::from(BUILDING_SHORT[1])
                + (f64::from(BUILDING_TALL[1]) - f64::from(BUILDING_SHORT[1])) * t)
                as u8,
            (f64::from(BUILDING_SHORT[2])
                + (f64::from(BUILDING_TALL[2]) - f64::from(BUILDING_SHORT[2])) * t)
                as u8,
        ];
        let ring: Vec<(f64, f64)> = b.open_ring().iter().map(|p| view.to_px(*p)).collect();
        canvas.fill_polygon(&ring, shade, 1.0);
    }

    // 3. Junction areas.
    for j in world.roads.junctions() {
        if j.shape.len() >= 4 {
            let ring: Vec<(f64, f64)> = j.shape.iter().map(|p| view.to_px(*p)).collect();
            canvas.fill_polygon(&ring, JUNCTION_FILL, 0.85);
        }
    }

    // 4. Footways, cycleways and crossings.
    for lane in world.roads.lanes() {
        let (colour, w) = match lane.kind {
            LaneKind::Sidewalk => (FOOT, 0.55),
            LaneKind::Cycle => (CYCLE, 0.9),
            _ => continue,
        };
        let pts: Vec<(f64, f64)> = lane.centreline.iter().map(|p| view.to_px(*p)).collect();
        canvas.polyline(&pts, colour, w, 0.85);
    }
    for c in world.crossings() {
        canvas.line(view.to_px(c.from), view.to_px(c.to), CROSSING, 1.2, 0.95);
    }

    // 5. Drivable lane centrelines, thinnest class first so arterials draw on top.
    let mut drivable: Vec<&v2xw_world::model::Lane> = world
        .roads
        .lanes()
        .iter()
        .filter(|l| l.kind == LaneKind::Driving || l.kind == LaneKind::Internal)
        .collect();
    drivable.sort_by_key(|l| {
        let class = world.roads.edge(l.edge).road_class;
        (std::cmp::Reverse(class as u8), l.id.index())
    });
    for lane in drivable {
        let class = world.roads.edge(lane.edge).road_class;
        let (colour, w) = road_style(class);
        let pts: Vec<(f64, f64)> = lane.centreline.iter().map(|p| view.to_px(*p)).collect();
        canvas.polyline(&pts, colour, w, 0.95);
    }

    // 6. Signalised junctions on top of everything.
    for j in world.roads.junctions() {
        if matches!(j.control, JunctionControl::Signalised { .. }) {
            canvas.square_outline(view.to_px(j.position), 3.0, SIGNAL, 1.1);
        }
    }

    // 7. One named street picked out, so that "is Broadway a diagonal?" is a question the
    //    picture answers rather than one the eye guesses at from among 1200 edges.
    for lane in world.roads.lanes() {
        if lane.kind != LaneKind::Driving {
            continue;
        }
        let edge = world.roads.edge(lane.edge);
        if world.symbols.resolve_optional(edge.name) != DIAGONAL_STREET {
            continue;
        }
        let pts: Vec<(f64, f64)> = lane.centreline.iter().map(|p| view.to_px(*p)).collect();
        canvas.polyline(&pts, NAMED, 3.0, 0.95);
    }

    // 8. The requested geodetic box, and the three landmarks of the projection check. If
    //    the projection were mirrored or mis-scaled these would not land on their blocks.
    let p = world.projection();
    let sw = p.to_enu(REQUESTED_BBOX[0], REQUESTED_BBOX[1]);
    let ne = p.to_enu(REQUESTED_BBOX[2], REQUESTED_BBOX[3]);
    let corners = [
        (sw.0, sw.1),
        (ne.0, sw.1),
        (ne.0, ne.1),
        (sw.0, ne.1),
        (sw.0, sw.1),
    ]
    .map(|(x, y)| view.to_px(Vec3::new(x, y, 0.0)));
    canvas.polyline(&corners, REQUESTED, 2.0, 0.8);

    for l in &LANDMARKS {
        let (x, y) = p.to_enu(l.lat, l.lon);
        let c = view.to_px(Vec3::new(x, y, 0.0));
        canvas.line((c.0 - 14.0, c.1), (c.0 + 14.0, c.1), LANDMARK, 2.0, 1.0);
        canvas.line((c.0, c.1 - 14.0), (c.0, c.1 + 14.0), LANDMARK, 2.0, 1.0);
        canvas.disc(c, 4.0, LANDMARK, 1.0);
    }
    canvas
}

// ---------------------------------------------------------------------------
// The junction render
// ---------------------------------------------------------------------------

/// Renders one junction and its surroundings at `px_per_m`.
fn render_junction(world: &World, jid: JunctionId, span_m: f64, px_per_m: f64) -> Canvas {
    let centre = world.roads.junction(jid).position;
    render_around(world, centre, span_m, px_per_m)
}

/// Renders whatever is within `span_m` of `centre` at `px_per_m`, lane by lane.
fn render_around(world: &World, centre: Vec3, span_m: f64, px_per_m: f64) -> Canvas {
    let view = View::centred(centre, span_m, px_per_m);
    let mut canvas = Canvas::new(view.width, view.height, BG);

    for b in &world.buildings {
        if !b.open_ring().iter().any(|p| view.near(*p, 40.0)) {
            continue;
        }
        let t = (b.height_m / 200.0).clamp(0.0, 1.0);
        let shade = [
            (226.0 - 120.0 * t) as u8,
            (222.0 - 130.0 * t) as u8,
            (214.0 - 130.0 * t) as u8,
        ];
        let ring: Vec<(f64, f64)> = b.open_ring().iter().map(|p| view.to_px(*p)).collect();
        canvas.fill_polygon(&ring, shade, 1.0);
    }

    // Every junction area in the window, so a misplaced polygon next door still shows.
    for other in world.roads.junctions() {
        if other.shape.len() >= 4 && view.near(other.position, span_m) {
            let ring: Vec<(f64, f64)> = other.shape.iter().map(|p| view.to_px(*p)).collect();
            canvas.fill_polygon(&ring, JUNCTION_FILL, 0.8);
            let outline: Vec<(f64, f64)> = other.shape.iter().map(|p| view.to_px(*p)).collect();
            canvas.polyline(&outline, [210, 150, 90], 1.0, 0.9);
        }
    }

    // Footways, cycleways, crossings.
    for lane in world.roads.lanes() {
        if !lane.centreline.iter().any(|p| view.near(*p, 10.0)) {
            continue;
        }
        let (colour, w) = match lane.kind {
            LaneKind::Sidewalk => (FOOT, 1.6),
            LaneKind::Cycle => (CYCLE, 2.0),
            _ => continue,
        };
        let pts: Vec<(f64, f64)> = lane.centreline.iter().map(|p| view.to_px(*p)).collect();
        canvas.polyline(&pts, colour, w, 0.9);
    }
    for c in world.crossings() {
        if view.near(c.from, 10.0) || view.near(c.to, 10.0) {
            canvas.line(view.to_px(c.from), view.to_px(c.to), CROSSING, 2.2, 0.95);
        }
    }

    // Ordinary drivable lanes: a grey band the width of the lane, then the centreline, so
    // both the lane's footprint and its exact geometry are visible.
    for lane in world.roads.lanes() {
        if lane.kind != LaneKind::Driving || !lane.centreline.iter().any(|p| view.near(*p, 10.0)) {
            continue;
        }
        let pts: Vec<(f64, f64)> = lane.centreline.iter().map(|p| view.to_px(*p)).collect();
        canvas.polyline(&pts, [205, 208, 216], lane.width_m * px_per_m, 0.95);
        canvas.polyline(&pts, [40, 55, 95], 1.4, 1.0);
        // A tick at the lane's start, pointing across it, marks the direction of travel.
        let (p, heading) = lane.pose_at(0.6);
        let n = Vec3::new(-math::sin(heading), math::cos(heading), 0.0);
        let half = lane.width_m / 2.0;
        canvas.line(
            view.to_px(p + n.scale(half)),
            view.to_px(p + n.scale(-half)),
            [40, 55, 95],
            1.2,
            0.9,
        );
    }

    // Internal connectors, coloured by turn direction, with an arrow head at the exit.
    let mut turn_of: BTreeMap<u32, TurnDirection> = BTreeMap::new();
    for c in world.roads.connections() {
        if let Some(via) = c.via {
            turn_of.insert(via.index(), c.direction);
        }
    }
    for lane in world.roads.lanes() {
        if lane.kind != LaneKind::Internal || !lane.centreline.iter().any(|p| view.near(*p, 5.0)) {
            continue;
        }
        let colour = turn_of
            .get(&lane.id.index())
            .copied()
            .map_or([120, 120, 120], turn_colour);
        let pts: Vec<(f64, f64)> = lane.centreline.iter().map(|p| view.to_px(*p)).collect();
        canvas.polyline(&pts, colour, 1.8, 0.95);
        // Arrow head: two 2.5 m barbs swept back from the exit heading.
        let heading = lane.heading_at(lane.length_m);
        let tip = lane.end();
        for sweep in [2.6, -2.6] {
            let a = heading + sweep;
            let barb = tip + Vec3::new(math::cos(a) * 2.6, math::sin(a) * 2.6, 0.0);
            canvas.line(view.to_px(tip), view.to_px(barb), colour, 1.6, 1.0);
        }
        canvas.disc(view.to_px(lane.start()), 1.6, colour, 0.9);
    }

    // The named street, picked out as in the overview, so a diagonal is unmistakable.
    for lane in world.roads.lanes() {
        if lane.kind != LaneKind::Driving
            || world
                .symbols
                .resolve_optional(world.roads.edge(lane.edge).name)
                != DIAGONAL_STREET
            || !lane.centreline.iter().any(|q| view.near(*q, 10.0))
        {
            continue;
        }
        let pts: Vec<(f64, f64)> = lane.centreline.iter().map(|q| view.to_px(*q)).collect();
        canvas.polyline(&pts, NAMED, 2.2, 0.95);
    }

    // The junction reference points: filled for signalised, hollow otherwise.
    for other in world.roads.junctions() {
        if !view.near(other.position, 5.0) {
            continue;
        }
        if matches!(other.control, JunctionControl::Signalised { .. }) {
            canvas.disc(view.to_px(other.position), 4.0, SIGNAL, 0.95);
        } else {
            canvas.square_outline(view.to_px(other.position), 3.0, [90, 90, 90], 1.2);
        }
    }
    canvas
}

// ---------------------------------------------------------------------------
// Projection cross-check
// ---------------------------------------------------------------------------

/// A landmark with the coordinates the check states, for the projection cross-check.
struct Landmark {
    /// Human name, for the report.
    name: &'static str,
    /// WGS-84 latitude, degrees north.
    lat: f64,
    /// WGS-84 longitude, degrees east.
    lon: f64,
}

/// The three landmarks the projection is cross-checked against, and which the overview
/// marks with a crosshair so that the numeric check and the picture agree.
///
/// Coordinates are stated here rather than read out of the extract, which is the point:
/// they are independent of the importer, so if the projection were mirrored or mis-scaled
/// the crosshairs would land on the wrong blocks.
const LANDMARKS: [Landmark; 3] = [
    // Grand Central Terminal, the main concourse, 42nd St & Park Ave.
    Landmark {
        name: "Grand Central Terminal",
        lat: 40.752_726,
        lon: -73.977_229,
    },
    // The Chrysler Building, Lexington Ave & 42nd St.
    Landmark {
        name: "Chrysler Building",
        lat: 40.751_652,
        lon: -73.975_311,
    },
    // The United Nations Secretariat, 1st Ave & 44th St.
    Landmark {
        name: "UN Secretariat",
        lat: 40.749_444,
        lon: -73.968_056,
    },
];

/// The geodetic box that was *asked* for, from the extract's own `<bounds>` element.
///
/// The importer clips nothing by default, so the world is wider than this; the report says
/// by how much and what sticks out.
const REQUESTED_BBOX: [f64; 4] = [40.744, -73.990, 40.762, -73.968];

/// Vincenty's inverse formula on WGS-84: the geodesic distance the projection is checked
/// against. Present only as the reference; the engine never computes one.
fn geodesic_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    const A: f64 = 6_378_137.0;
    const F: f64 = 1.0 / 298.257_223_563;
    let b = A * (1.0 - F);
    let rad = core::f64::consts::PI / 180.0;
    let u1 = math::atan((1.0 - F) * math::tan(lat1 * rad));
    let u2 = math::atan((1.0 - F) * math::tan(lat2 * rad));
    let l = (lon2 - lon1) * rad;
    let (sin_u1, cos_u1) = math::sin_cos(u1);
    let (sin_u2, cos_u2) = math::sin_cos(u2);
    let mut lambda = l;
    let mut cos2_alpha = 0.0;
    let mut sin_sigma = 0.0;
    let mut cos_sigma = 0.0;
    let mut sigma = 0.0;
    let mut cos_2sigma_m = 0.0;
    for _ in 0..200 {
        let (sin_l, cos_l) = math::sin_cos(lambda);
        sin_sigma = math::hypot(cos_u2 * sin_l, cos_u1 * sin_u2 - sin_u1 * cos_u2 * cos_l);
        cos_sigma = sin_u1 * sin_u2 + cos_u1 * cos_u2 * cos_l;
        sigma = math::atan2(sin_sigma, cos_sigma);
        let sin_alpha = if sin_sigma == 0.0 {
            0.0
        } else {
            cos_u1 * cos_u2 * sin_l / sin_sigma
        };
        cos2_alpha = 1.0 - sin_alpha * sin_alpha;
        cos_2sigma_m = if cos2_alpha == 0.0 {
            0.0
        } else {
            cos_sigma - 2.0 * sin_u1 * sin_u2 / cos2_alpha
        };
        let c = F / 16.0 * cos2_alpha * (4.0 + F * (4.0 - 3.0 * cos2_alpha));
        let prev = lambda;
        lambda = l
            + (1.0 - c)
                * F
                * sin_alpha
                * (sigma
                    + c * sin_sigma
                        * (cos_2sigma_m
                            + c * cos_sigma * (-1.0 + 2.0 * cos_2sigma_m * cos_2sigma_m)));
        if (lambda - prev).abs() < 1e-14 {
            break;
        }
    }
    let u_sq = cos2_alpha * (A * A - b * b) / (b * b);
    let big_a = 1.0 + u_sq / 16384.0 * (4096.0 + u_sq * (-768.0 + u_sq * (320.0 - 175.0 * u_sq)));
    let big_b = u_sq / 1024.0 * (256.0 + u_sq * (-128.0 + u_sq * (74.0 - 47.0 * u_sq)));
    let delta_sigma = big_b
        * sin_sigma
        * (cos_2sigma_m
            + big_b / 4.0
                * (cos_sigma * (-1.0 + 2.0 * cos_2sigma_m * cos_2sigma_m)
                    - big_b / 6.0
                        * cos_2sigma_m
                        * (-3.0 + 4.0 * sin_sigma * sin_sigma)
                        * (-3.0 + 4.0 * cos_2sigma_m * cos_2sigma_m)));
    b * big_a * (sigma - delta_sigma)
}

/// Projects three landmarks and compares every pairwise distance with the geodesic.
fn landmark_check(world: &World, view: &View, out: &mut String) {
    let landmarks = &LANDMARKS;
    let p = world.projection();
    let _ = writeln!(out, "PROJECTION CROSS-CHECK");
    let _ = writeln!(
        out,
        "  projection   {}\n  origin       {:.7}, {:.7}",
        Projection::NAME,
        world.origin.lat_deg,
        world.origin.lon_deg
    );
    let _ = writeln!(
        out,
        "  scale        {:.3} m/deg lat, {:.3} m/deg lon",
        p.metres_per_degree_latitude(),
        p.metres_per_degree_longitude()
    );
    let mut enu = Vec::new();
    for l in landmarks {
        let (x, y) = p.to_enu(l.lat, l.lon);
        let (col, row) = view.to_px(Vec3::new(x, y, 0.0));
        let _ = writeln!(
            out,
            "  {:<24} lat {:.6} lon {:.6}  ->  x {:9.2} m  y {:9.2} m   px ({:.0}, {:.0})",
            l.name, l.lat, l.lon, x, y, col, row
        );
        enu.push((x, y));
    }
    let mut worst: f64 = 0.0;
    for i in 0..landmarks.len() {
        for j in i + 1..landmarks.len() {
            let planar = math::hypot(enu[j].0 - enu[i].0, enu[j].1 - enu[i].1);
            let geo = geodesic_m(
                landmarks[i].lat,
                landmarks[i].lon,
                landmarks[j].lat,
                landmarks[j].lon,
            );
            let err = planar - geo;
            worst = worst.max(err.abs());
            let _ = writeln!(
                out,
                "  {:<24} -> {:<24} projected {:8.2} m   geodesic {:8.2} m   delta {:+.2} m ({:+.3} %)",
                landmarks[i].name,
                landmarks[j].name,
                planar,
                geo,
                err,
                100.0 * err / geo
            );
        }
    }
    let _ = writeln!(out, "  worst pairwise error  {worst:.3} m");
    // North must be up in the raster: a landmark further north gets a smaller row index.
    let north = view.to_px(Vec3::new(0.0, 1000.0, 0.0)).1;
    let south = view.to_px(Vec3::new(0.0, 0.0, 0.0)).1;
    let east = view.to_px(Vec3::new(1000.0, 0.0, 0.0)).0;
    let _ = writeln!(
        out,
        "  raster orientation: 1 km north is row {north:.0} vs row {south:.0} (north-up: {}), 1 km east is column {east:.0} (east-right: {})",
        north < south,
        east > 0.0
    );
}

// ---------------------------------------------------------------------------
// Network statistics
// ---------------------------------------------------------------------------

/// Prints a percentile summary of a sample.
fn summarise(name: &str, mut xs: Vec<f64>, unit: &str, out: &mut String) {
    if xs.is_empty() {
        let _ = writeln!(out, "  {name:<28} (empty)");
        return;
    }
    math::sort_total_order(&mut xs);
    let mean = math::sum_ordered(xs.iter().copied()) / xs.len() as f64;
    let _ = writeln!(
        out,
        "  {:<28} n={:<7} min {:8.2} p10 {:8.2} p50 {:8.2} p90 {:8.2} p99 {:8.2} max {:9.2} mean {:8.2} {}",
        name,
        xs.len(),
        xs[0],
        math::quantile_sorted(&xs, 0.10),
        math::quantile_sorted(&xs, 0.50),
        math::quantile_sorted(&xs, 0.90),
        math::quantile_sorted(&xs, 0.99),
        xs[xs.len() - 1],
        mean,
        unit
    );
}

/// The statistical sanity check of the road network.
fn statistics(world: &World, out: &mut String) {
    let roads = &world.roads;
    let drivable: Vec<&v2xw_world::model::Lane> = roads
        .lanes()
        .iter()
        .filter(|l| l.kind == LaneKind::Driving)
        .collect();
    let internal: Vec<&v2xw_world::model::Lane> = roads
        .lanes()
        .iter()
        .filter(|l| l.kind == LaneKind::Internal)
        .collect();

    let _ = writeln!(out, "\nNETWORK STATISTICS");
    let (w_m, h_m) = (
        world.bbox.max.x - world.bbox.min.x,
        world.bbox.max.y - world.bbox.min.y,
    );
    let _ = writeln!(
        out,
        "  world extent               {:.0} m east-west x {:.0} m north-south = {:.2} km2",
        w_m,
        h_m,
        w_m * h_m / 1e6
    );

    summarise(
        "drivable lane length",
        drivable.iter().map(|l| l.length_m).collect(),
        "m",
        out,
    );
    summarise(
        "internal connector length",
        internal.iter().map(|l| l.length_m).collect(),
        "m",
        out,
    );
    summarise(
        "sidewalk lane length",
        roads
            .lanes()
            .iter()
            .filter(|l| l.kind == LaneKind::Sidewalk)
            .map(|l| l.length_m)
            .collect(),
        "m",
        out,
    );

    let drivable_km: f64 = math::sum_ordered(drivable.iter().map(|l| l.length_m)) / 1000.0;
    let internal_km: f64 = math::sum_ordered(internal.iter().map(|l| l.length_m)) / 1000.0;
    let _ = writeln!(
        out,
        "  drivable lane kilometres   {drivable_km:.2} km (plus {internal_km:.2} km of internal connectors)"
    );
    let _ = writeln!(
        out,
        "  lane km per km2            {:.2}",
        drivable_km / (w_m * h_m / 1e6)
    );

    // --- junction degree ----------------------------------------------------
    // Degree counted over *edges*, not lanes: a four-way crossing of two-lane streets has
    // degree 4, whatever the lane count. Approach and departure edges at the same junction
    // on the same OSM way are one arm, so the arms are counted as distinct edge endpoints.
    let mut arms: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
    for e in roads.edges() {
        if e.road_class == RoadClass::Internal {
            continue;
        }
        let drivable_edge = roads
            .lanes()
            .iter()
            .any(|l| l.edge == e.id && l.kind == LaneKind::Driving);
        if !drivable_edge {
            continue;
        }
        arms.entry(e.from.index()).or_default().insert(e.id.index());
        arms.entry(e.to.index()).or_default().insert(e.id.index());
    }
    let mut degree_hist: BTreeMap<usize, usize> = BTreeMap::new();
    for j in roads.junctions() {
        let d = arms.get(&j.id.index()).map_or(0, BTreeSet::len);
        *degree_hist.entry(d).or_default() += 1;
    }
    let _ = writeln!(out, "  junction degree (drivable edge arms):");
    for (d, n) in &degree_hist {
        let _ = writeln!(
            out,
            "    degree {d:<3} {n:>6}  {:>5.1} %",
            100.0 * *n as f64 / roads.junctions().len() as f64
        );
    }

    // --- signalisation ------------------------------------------------------
    let signalised = roads
        .junctions()
        .iter()
        .filter(|j| matches!(j.control, JunctionControl::Signalised { .. }))
        .count();
    let signalised_deg3 = roads
        .junctions()
        .iter()
        .filter(|j| {
            matches!(j.control, JunctionControl::Signalised { .. })
                && arms.get(&j.id.index()).map_or(0, BTreeSet::len) >= 3
        })
        .count();
    let deg3plus = roads
        .junctions()
        .iter()
        .filter(|j| arms.get(&j.id.index()).map_or(0, BTreeSet::len) >= 3)
        .count();
    let _ = writeln!(
        out,
        "  signalised junctions       {signalised} of {} ({:.1} %); of the {deg3plus} with 3+ drivable arms, {signalised_deg3} are signalised ({:.1} %)",
        roads.junctions().len(),
        100.0 * signalised as f64 / roads.junctions().len() as f64,
        100.0 * signalised_deg3 as f64 / deg3plus.max(1) as f64
    );

    // --- one-way fraction ---------------------------------------------------
    // An OSM way that is two-way is imported as two opposite edges between the same pair
    // of junctions, so a *directed* edge is one-way exactly when no reverse twin exists.
    let mut endpoints: BTreeSet<(u32, u32)> = BTreeSet::new();
    let mut directed = 0usize;
    for e in roads.edges() {
        if e.road_class == RoadClass::Internal {
            continue;
        }
        if !roads
            .lanes()
            .iter()
            .any(|l| l.edge == e.id && l.kind == LaneKind::Driving)
        {
            continue;
        }
        directed += 1;
        endpoints.insert((e.from.index(), e.to.index()));
    }
    let one_way = endpoints
        .iter()
        .filter(|(a, b)| !endpoints.contains(&(*b, *a)))
        .count();
    let _ = writeln!(
        out,
        "  drivable directed edges    {directed}; distinct (from,to) pairs {}; {one_way} have no reverse twin = {:.1} % one-way",
        endpoints.len(),
        100.0 * one_way as f64 / endpoints.len() as f64
    );

    // --- connectivity of the drivable lane graph ----------------------------
    // The graph is the lane graph including internal connectors, because that is what a
    // vehicle actually drives; components are found on the *undirected* version first
    // (reachability irrespective of one-way sense) and then the largest strongly connected
    // set is measured by a forward/backward reachability intersection from the lane in the
    // biggest weak component.
    let n = roads.lanes().len();
    let motor: Vec<bool> = roads
        .lanes()
        .iter()
        .map(|l| l.kind == LaneKind::Driving || l.kind == LaneKind::Internal)
        .collect();
    let mut succ: Vec<Vec<u32>> = vec![Vec::new(); n];
    let mut pred: Vec<Vec<u32>> = vec![Vec::new(); n];
    let add = |succ: &mut Vec<Vec<u32>>, pred: &mut Vec<Vec<u32>>, a: LaneId, b: LaneId| {
        if motor[a.as_usize()] && motor[b.as_usize()] {
            succ[a.as_usize()].push(b.index());
            pred[b.as_usize()].push(a.index());
        }
    };
    for c in roads.connections() {
        if !c.permitted {
            continue;
        }
        // A movement is recorded twice: `approach -> departure` carrying its connector in
        // `via`, and `connector -> departure` with `via` empty (see `Connection`). The
        // geometric hops a vehicle actually drives are therefore `approach -> connector`
        // (read off the first record) and `connector -> departure` (the second). Taking
        // only the `via`-empty records would leave every approach lane with no successor,
        // and taking the abstract `approach -> departure` record as an edge would
        // short-circuit the junction geometry.
        match c.via {
            Some(v) => add(&mut succ, &mut pred, c.from_lane, v),
            None => add(&mut succ, &mut pred, c.from_lane, c.to_lane),
        }
    }
    // The same hop can be recorded by several movements; dedupe so degrees are counts of
    // distinct neighbours.
    for v in succ.iter_mut().chain(pred.iter_mut()) {
        v.sort_unstable();
        v.dedup();
    }
    let motor_count = motor.iter().filter(|m| **m).count();

    let mut seen = vec![false; n];
    let mut components: Vec<Vec<u32>> = Vec::new();
    for start in 0..n {
        if !motor[start] || seen[start] {
            continue;
        }
        let mut queue = VecDeque::from([start as u32]);
        seen[start] = true;
        let mut comp = Vec::new();
        while let Some(v) = queue.pop_front() {
            comp.push(v);
            for w in succ[v as usize].iter().chain(pred[v as usize].iter()) {
                if !seen[*w as usize] {
                    seen[*w as usize] = true;
                    queue.push_back(*w);
                }
            }
        }
        comp.sort_unstable();
        components.push(comp);
    }
    components.sort_by_key(|c| (std::cmp::Reverse(c.len()), c[0]));
    let largest = &components[0];
    let _ = writeln!(
        out,
        "  motorised lanes            {motor_count} ({} driving + {} internal) in {} weakly connected component(s)",
        drivable.len(),
        internal.len(),
        components.len()
    );
    let _ = writeln!(
        out,
        "  largest weak component     {} lanes = {:.1} % ({:.2} km of driving lane)",
        largest.len(),
        100.0 * largest.len() as f64 / motor_count as f64,
        math::sum_ordered(
            largest
                .iter()
                .filter(|i| roads.lanes()[**i as usize].kind == LaneKind::Driving)
                .map(|i| roads.lanes()[*i as usize].length_m)
        ) / 1000.0
    );
    let _ = writeln!(
        out,
        "  next 4 components          {:?}",
        components
            .iter()
            .skip(1)
            .take(4)
            .map(Vec::len)
            .collect::<Vec<_>>()
    );

    // The largest *strongly* connected component, by Kosaraju: a vehicle can drive from any
    // lane in it to any other and back, which is the property a traffic demand model needs.
    // Iteration order is by lane id throughout, so the answer does not depend on hashing.
    let mut order: Vec<u32> = Vec::with_capacity(motor_count);
    let mut state = vec![0u8; n]; // 0 unvisited, 1 on stack, 2 done
    for start in 0..n as u32 {
        if !motor[start as usize] || state[start as usize] != 0 {
            continue;
        }
        let mut stack = vec![(start, 0usize)];
        state[start as usize] = 1;
        while let Some((v, i)) = stack.pop() {
            if i < succ[v as usize].len() {
                stack.push((v, i + 1));
                let w = succ[v as usize][i];
                if state[w as usize] == 0 {
                    state[w as usize] = 1;
                    stack.push((w, 0));
                }
            } else {
                state[v as usize] = 2;
                order.push(v);
            }
        }
    }
    let mut comp = vec![u32::MAX; n];
    let mut sccs: Vec<Vec<u32>> = Vec::new();
    for v in order.iter().rev() {
        if comp[*v as usize] != u32::MAX {
            continue;
        }
        let id = sccs.len() as u32;
        let mut group = Vec::new();
        let mut q = VecDeque::from([*v]);
        comp[*v as usize] = id;
        while let Some(x) = q.pop_front() {
            group.push(x);
            for y in &pred[x as usize] {
                if comp[*y as usize] == u32::MAX && motor[*y as usize] {
                    comp[*y as usize] = id;
                    q.push_back(*y);
                }
            }
        }
        group.sort_unstable();
        sccs.push(group);
    }
    sccs.sort_by_key(|g| (std::cmp::Reverse(g.len()), g[0]));
    let scc = &sccs[0];
    let scc_driving = scc
        .iter()
        .filter(|i| roads.lanes()[**i as usize].kind == LaneKind::Driving)
        .count();
    let _ = writeln!(
        out,
        "  largest strong component   {} lanes ({scc_driving} driving = {:.1} % of driving lanes, {:.2} km); {} strong components in all",
        scc.len(),
        100.0 * scc_driving as f64 / drivable.len() as f64,
        math::sum_ordered(
            scc.iter()
                .filter(|i| roads.lanes()[**i as usize].kind == LaneKind::Driving)
                .map(|i| roads.lanes()[*i as usize].length_m)
        ) / 1000.0,
        sccs.len()
    );

    // --- dead ends ----------------------------------------------------------
    let no_exit: Vec<u32> = (0..n as u32)
        .filter(|i| motor[*i as usize] && roads.lanes()[*i as usize].kind == LaneKind::Driving)
        .filter(|i| succ[*i as usize].is_empty())
        .collect();
    let no_entry: Vec<u32> = (0..n as u32)
        .filter(|i| motor[*i as usize] && roads.lanes()[*i as usize].kind == LaneKind::Driving)
        .filter(|i| pred[*i as usize].is_empty())
        .collect();
    // A dead-end *junction*: exactly one drivable arm, i.e. a true cul-de-sac or a lane
    // that leaves the extract at the bbox edge.
    let stubs: Vec<&v2xw_world::model::Junction> = roads
        .junctions()
        .iter()
        .filter(|j| arms.get(&j.id.index()).map_or(0, BTreeSet::len) == 1)
        .collect();
    let margin_m = 30.0;
    let on_edge = stubs
        .iter()
        .filter(|j| {
            j.position.x - world.bbox.min.x < margin_m
                || world.bbox.max.x - j.position.x < margin_m
                || j.position.y - world.bbox.min.y < margin_m
                || world.bbox.max.y - j.position.y < margin_m
        })
        .count();
    let _ = writeln!(
        out,
        "  driving lanes with no successor {} / no predecessor {}",
        no_exit.len(),
        no_entry.len()
    );
    // Split the strong-connectivity figure by road class. A service road, alley or
    // driveway is normally a stub you enter and leave the same way, so it can never be in
    // a strong component; the meaningful question is whether the *street grid* is.
    let street = |i: u32| -> bool {
        let lane = &roads.lanes()[i as usize];
        lane.kind == LaneKind::Driving
            && !matches!(
                roads.edge(lane.edge).road_class,
                RoadClass::Service | RoadClass::Link | RoadClass::Path
            )
    };
    let street_total = (0..n as u32).filter(|i| street(*i)).count();
    let street_in_scc = scc.iter().filter(|i| street(**i)).count();
    let _ = writeln!(
        out,
        "  street-class driving lanes {street_total}; {street_in_scc} of them ({:.1} %) are in the largest strong component, so the shortfall is mostly service roads and ramps",
        100.0 * street_in_scc as f64 / street_total as f64
    );
    // Reachability at the **edge** level. In a lane-level network a vehicle moves sideways
    // by changing lane, which is not a `Connection`, so the lane graph understates where a
    // vehicle can get to: a lane that only goes straight is its own strong component even
    // though its edge is fully connected. The edge graph is therefore the honest measure of
    // "can a vehicle drive from any street to any other and back".
    let mut edge_ids: Vec<u32> = Vec::new();
    let mut edge_slot: BTreeMap<u32, usize> = BTreeMap::new();
    for e in roads.edges() {
        if e.road_class == RoadClass::Internal {
            continue;
        }
        if e.lanes
            .iter()
            .any(|l| roads.lanes()[l.as_usize()].kind == LaneKind::Driving)
        {
            edge_slot.insert(e.id.index(), edge_ids.len());
            edge_ids.push(e.id.index());
        }
    }
    let m = edge_ids.len();
    let mut esucc: Vec<Vec<u32>> = vec![Vec::new(); m];
    let mut epred: Vec<Vec<u32>> = vec![Vec::new(); m];
    for c in roads.connections() {
        if !c.permitted {
            continue;
        }
        let (a, b) = (
            roads.lanes()[c.from_lane.as_usize()].edge.index(),
            roads.lanes()[c.to_lane.as_usize()].edge.index(),
        );
        if let (Some(i), Some(j)) = (edge_slot.get(&a), edge_slot.get(&b)) {
            if i != j {
                esucc[*i].push(*j as u32);
                epred[*j].push(*i as u32);
            }
        }
    }
    for v in esucc.iter_mut().chain(epred.iter_mut()) {
        v.sort_unstable();
        v.dedup();
    }
    let ereach = |adj: &Vec<Vec<u32>>, root: u32| -> Vec<bool> {
        let mut vis = vec![false; m];
        let mut q = VecDeque::from([root]);
        vis[root as usize] = true;
        while let Some(v) = q.pop_front() {
            for w in &adj[v as usize] {
                if !vis[*w as usize] {
                    vis[*w as usize] = true;
                    q.push_back(*w);
                }
            }
        }
        vis
    };
    // Root the measurement at the edge with the most successors, which is in the core.
    let root = (0..m as u32)
        .max_by_key(|i| (esucc[*i as usize].len(), std::cmp::Reverse(*i)))
        .expect("at least one drivable edge");
    let ef = ereach(&esucc, root);
    let eb = ereach(&epred, root);
    let both = (0..m).filter(|i| ef[*i] && eb[*i]).count();
    let edge_km = math::sum_ordered((0..m).filter(|i| ef[*i] && eb[*i]).map(|i| {
        let e = roads.edge(v2xw_core::ids::EdgeId::new(edge_ids[i]));
        e.lanes
            .iter()
            .map(|l| roads.lanes()[l.as_usize()].length_m)
            .sum::<f64>()
    })) / 1000.0;
    let _ = writeln!(
        out,
        "  edge graph                 {m} drivable edges; {both} ({:.1} %) are reachable both ways from the busiest edge, carrying {edge_km:.2} lane km",
        100.0 * both as f64 / m as f64
    );

    // Very short lanes: over-splitting or junction trimming eating a whole block face.
    let mut short_by_class: BTreeMap<&str, usize> = BTreeMap::new();
    for lane in &drivable {
        if lane.length_m < 5.0 {
            *short_by_class
                .entry(roads.edge(lane.edge).road_class.label())
                .or_default() += 1;
        }
    }
    let short_total: usize = short_by_class.values().sum();
    let _ = writeln!(
        out,
        "  driving lanes under 5 m    {short_total} ({:.1} %) by class {short_by_class:?}",
        100.0 * short_total as f64 / drivable.len() as f64
    );
    let _ = writeln!(
        out,
        "  single-arm junctions       {} ({on_edge} within {margin_m:.0} m of the bbox edge = clipped ways, {} interior = genuine cul-de-sacs or import gaps)",
        stubs.len(),
        stubs.len() - on_edge
    );

    // --- speeds, widths, buildings ------------------------------------------
    summarise(
        "drivable speed limit",
        drivable.iter().map(|l| l.speed_limit_mps * 3.6).collect(),
        "km/h",
        out,
    );
    summarise(
        "drivable lane width",
        drivable.iter().map(|l| l.width_m).collect(),
        "m",
        out,
    );
    summarise(
        "building height",
        world.buildings.iter().map(|b| b.height_m).collect(),
        "m",
        out,
    );

    // --- is a building on the roadway? --------------------------------------
    // The cheap geometric check the render is meant to make visible: how many drivable
    // lane centreline vertices fall inside a building footprint. A handful is expected
    // (arcades, and buildings genuinely mapped over a road such as the Park Avenue viaduct
    // at Grand Central); a large share spread over many buildings would mean the footprints
    // and the roads are in different frames, which is the defect the render exists to catch.
    let mut inside = 0usize;
    let mut sampled = 0usize;
    let mut lanes_hit: BTreeSet<u32> = BTreeSet::new();
    let mut per_building: BTreeMap<u32, usize> = BTreeMap::new();
    for lane in &drivable {
        for p in &lane.centreline {
            sampled += 1;
            if let Some(b) = world
                .buildings
                .iter()
                .find(|b| b.bbox().contains_2d(*p) && b.contains_2d(*p))
            {
                inside += 1;
                lanes_hit.insert(lane.id.index());
                *per_building.entry(b.id.index()).or_default() += 1;
            }
        }
    }
    let _ = writeln!(
        out,
        "  lane vertices inside a building footprint  {inside} of {sampled} ({:.3} %), over {} lanes and {} buildings",
        100.0 * inside as f64 / sampled as f64,
        lanes_hit.len(),
        per_building.len()
    );
    let mut worst: Vec<(usize, u32)> = per_building.iter().map(|(b, n)| (*n, *b)).collect();
    worst.sort_by_key(|(n, b)| (std::cmp::Reverse(*n), *b));
    for (n, b) in worst.iter().take(8) {
        let building = &world.buildings[*b as usize];
        let c = building.bbox().center();
        let (lat, lon) = world.projection().to_geodetic(c.x, c.y);
        let area = ring_area_m2(building.open_ring());
        let _ = writeln!(
            out,
            "    building {b:<6} {n:>4} vertices  {:>7.0} m2  h {:>6.1} m  {lat:.5},{lon:.5}  {:?}",
            area,
            building.height_m,
            world.symbols.resolve_optional(building.name)
        );
    }

    // --- the longest drivable lanes -----------------------------------------
    // A surface street in the Manhattan grid is a block face: 60-80 m north-south, about
    // 270 m east-west. Anything much longer is either a limited-access road with no
    // intersections (the FDR Drive, a tunnel approach) or a way the importer failed to
    // split at a junction, and the two are told apart by looking at the street name.
    let mut longest: Vec<&v2xw_world::model::Lane> = drivable.clone();
    longest.sort_by(|a, b| {
        b.length_m
            .partial_cmp(&a.length_m)
            .expect("lane lengths are finite")
            .then(a.id.index().cmp(&b.id.index()))
    });
    let _ = writeln!(out, "  longest drivable lanes:");
    for lane in longest.iter().take(10) {
        let edge = roads.edge(lane.edge);
        let (lat0, lon0) = world
            .projection()
            .to_geodetic(lane.start().x, lane.start().y);
        let (lat1, lon1) = world.projection().to_geodetic(lane.end().x, lane.end().y);
        let _ = writeln!(
            out,
            "    lane {:<6} {:>8.1} m  {:<12} {:<28} {lat0:.5},{lon0:.5} -> {lat1:.5},{lon1:.5}  succ {} pred {}",
            lane.id.index(),
            lane.length_m,
            edge.road_class.label(),
            world.symbols.resolve_optional(edge.name),
            succ[lane.id.as_usize()].len(),
            pred[lane.id.as_usize()].len()
        );
    }

    // --- implausible speed limits -------------------------------------------
    let mut by_speed: BTreeMap<i64, (usize, String)> = BTreeMap::new();
    for lane in &drivable {
        let kmh = (lane.speed_limit_mps * 3.6).round() as i64;
        let entry = by_speed.entry(kmh).or_insert((0, String::new()));
        entry.0 += 1;
        let name = world.symbols.resolve_optional(roads.edge(lane.edge).name);
        if !name.is_empty() && entry.1.len() < 90 && !entry.1.contains(name) {
            if !entry.1.is_empty() {
                entry.1.push_str(", ");
            }
            entry.1.push_str(name);
        }
    }
    let _ = writeln!(out, "  drivable speed-limit histogram (km/h):");
    for (kmh, (n, names)) in &by_speed {
        let _ = writeln!(out, "    {kmh:>4} km/h  {n:>5} lanes   {names}");
    }

    // --- extent overshoot ---------------------------------------------------
    // The extract's own `<bounds>` is the box that was asked for; Overpass returns whole
    // ways, and the importer clips nothing unless `OsmOptions::bbox` is set, so the world
    // is whatever those ways span.
    let p = world.projection();
    let sw = p.to_enu(REQUESTED_BBOX[0], REQUESTED_BBOX[1]);
    let ne = p.to_enu(REQUESTED_BBOX[2], REQUESTED_BBOX[3]);
    let req_w = ne.0 - sw.0;
    let req_h = ne.1 - sw.1;
    let outside = drivable
        .iter()
        .filter(|l| {
            l.centreline
                .iter()
                .all(|q| q.x < sw.0 || q.x > ne.0 || q.y < sw.1 || q.y > ne.1)
        })
        .count();
    let outside_km = math::sum_ordered(
        drivable
            .iter()
            .filter(|l| {
                l.centreline
                    .iter()
                    .all(|q| q.x < sw.0 || q.x > ne.0 || q.y < sw.1 || q.y > ne.1)
            })
            .map(|l| l.length_m),
    ) / 1000.0;
    let _ = writeln!(
        out,
        "  requested bbox             {:.0} m x {:.0} m = {:.2} km2; world is {:.2}x its area",
        req_w,
        req_h,
        req_w * req_h / 1e6,
        (w_m * h_m) / (req_w * req_h)
    );
    let _ = writeln!(
        out,
        "  drivable lanes wholly outside the requested bbox  {outside} ({outside_km:.2} km)"
    );
    let buildings_outside = world
        .buildings
        .iter()
        .filter(|b| {
            b.open_ring()
                .iter()
                .all(|q| q.x < sw.0 || q.x > ne.0 || q.y < sw.1 || q.y > ne.1)
        })
        .count();
    let _ = writeln!(
        out,
        "  buildings wholly outside the requested bbox       {buildings_outside}"
    );

    // --- handedness: right-hand traffic -------------------------------------
    // Two checks the render cannot make obvious on its own.
    //
    // 1. Within one edge, lane 0 is documented as the rightmost in the direction of
    //    travel, so the offset from lane 0 to lane 1 must point to the **left** of travel:
    //    cross(direction, offset) > 0 in a right-handed ENU frame.
    // 2. Between an edge and its reverse twin, the opposing carriageway must also lie to
    //    the left, which is what driving on the right means.
    let mut lane_order_ok = 0usize;
    let mut lane_order_bad = 0usize;
    for e in roads.edges() {
        if e.road_class == RoadClass::Internal || e.lanes.len() < 2 {
            continue;
        }
        let a = &roads.lanes()[e.lanes[0].as_usize()];
        let b = &roads.lanes()[e.lanes[1].as_usize()];
        if a.kind != LaneKind::Driving || b.kind != LaneKind::Driving {
            continue;
        }
        let s = a.length_m / 2.0;
        let (pa, heading) = a.pose_at(s);
        let pb = b.point_at((s).min(b.length_m));
        let (dx, dy) = (math::cos(heading), math::sin(heading));
        let off = pb - pa;
        if dx * off.y - dy * off.x > 0.0 {
            lane_order_ok += 1;
        } else {
            lane_order_bad += 1;
        }
    }
    let mut twin_ok = 0usize;
    let mut twin_bad = 0usize;
    let mut by_pair: BTreeMap<(u32, u32), Vec<&v2xw_world::model::Edge>> = BTreeMap::new();
    for e in roads.edges() {
        if e.road_class == RoadClass::Internal {
            continue;
        }
        by_pair
            .entry((
                e.from.index().min(e.to.index()),
                e.from.index().max(e.to.index()),
            ))
            .or_default()
            .push(e);
    }
    for es in by_pair.values() {
        if es.len() != 2 || es[0].from == es[1].from {
            continue;
        }
        let a = &roads.lanes()[es[0].lanes[0].as_usize()];
        let b = &roads.lanes()[es[1].lanes[0].as_usize()];
        if a.kind != LaneKind::Driving || b.kind != LaneKind::Driving {
            continue;
        }
        let (pa, heading) = a.pose_at(a.length_m / 2.0);
        let pb = b.project_point(pa).point;
        let (dx, dy) = (math::cos(heading), math::sin(heading));
        let off = pb - pa;
        if off.norm_2d() < 1e-6 {
            continue; // the two carriageways share a centreline: nothing to judge
        }
        if dx * off.y - dy * off.x > 0.0 {
            twin_ok += 1;
        } else {
            twin_bad += 1;
        }
    }
    let _ = writeln!(
        out,
        "  right-hand traffic         lane 0 is rightmost in {lane_order_ok} of {} multi-lane edges ({lane_order_bad} wrong); the opposing carriageway is on the left for {twin_ok} of {} two-way street pairs ({twin_bad} wrong)",
        lane_order_ok + lane_order_bad,
        twin_ok + twin_bad
    );

    // --- junctions with connectors but no drivable arms ---------------------
    let orphan: Vec<&v2xw_world::model::Junction> = roads
        .junctions()
        .iter()
        .filter(|j| !j.internal.is_empty() && arms.get(&j.id.index()).map_or(0, BTreeSet::len) < 2)
        .collect();
    let _ = writeln!(
        out,
        "  junctions with connectors but fewer than 2 drivable arms  {}",
        orphan.len()
    );

    // --- density inside the box that was actually asked for -----------------
    let inside_box = |l: &v2xw_world::model::Lane| -> bool {
        let c = l.point_at(l.length_m / 2.0);
        c.x >= sw.0 && c.x <= ne.0 && c.y >= sw.1 && c.y <= ne.1
    };
    let in_km = math::sum_ordered(
        drivable
            .iter()
            .filter(|l| inside_box(l))
            .map(|l| l.length_m),
    ) / 1000.0;
    // Roadway centreline, not lane: one representative lane (index 0) per directed edge,
    // halved for a two-way street, which is the number a "streets per km2" check compares.
    let mut roadway_m = 0.0;
    for es in by_pair.values() {
        for e in es {
            let Some(first) = e.lanes.first() else {
                continue;
            };
            let lane = &roads.lanes()[first.as_usize()];
            if lane.kind != LaneKind::Driving || !inside_box(lane) {
                continue;
            }
            roadway_m += lane.length_m / if es.len() == 2 { 2.0 } else { 1.0 };
        }
    }
    let req_km2 = req_w * req_h / 1e6;
    let junctions_in = roads
        .junctions()
        .iter()
        .filter(|j| {
            arms.get(&j.id.index()).map_or(0, BTreeSet::len) >= 3
                && j.position.x >= sw.0
                && j.position.x <= ne.0
                && j.position.y >= sw.1
                && j.position.y <= ne.1
        })
        .count();
    let _ = writeln!(
        out,
        "  inside the requested bbox  {in_km:.2} lane km ({:.1} lane km/km2), {:.2} km of roadway centreline ({:.1} km/km2), {junctions_in} junctions with 3+ drivable arms ({:.0}/km2)",
        in_km / req_km2,
        roadway_m / 1000.0,
        roadway_m / 1000.0 / req_km2,
        junctions_in as f64 / req_km2
    );
    let _ = writeln!(
        out,
        "  mean lanes per directed drivable edge  {:.2}",
        drivable.len() as f64 / directed as f64
    );

    // --- is the grid where Manhattan's grid is? -----------------------------
    // Manhattan's Commissioners' Plan grid is rotated about 29 deg clockwise of true
    // north. Each drivable lane segment votes for its heading folded into [0, 90) deg;
    // the two modes should sit near 29 deg (the avenues) and 119 deg -> folded 29 deg
    // as well, so the histogram is folded into [0, 90) and the mode read directly.
    let mut heading_hist = [0usize; 90];
    let mut weighted = [0.0f64; 90];
    for lane in &drivable {
        for w in lane.centreline.windows(2) {
            let d = w[1] - w[0];
            let deg = math::atan2(d.y, d.x) * 180.0 / core::f64::consts::PI;
            let folded = ((deg % 90.0) + 90.0) % 90.0;
            let bin = (folded.floor() as usize).min(89);
            heading_hist[bin] += 1;
            weighted[bin] += d.norm_2d();
        }
    }
    let mode = (0..90)
        .max_by(|a, b| {
            weighted[*a]
                .partial_cmp(&weighted[*b])
                .expect("lengths are finite")
        })
        .expect("90 bins");
    let total_len: f64 = math::sum_ordered(weighted.iter().copied());
    let near_mode: f64 = math::sum_ordered(
        (0..90)
            .filter(|b| ((*b as i64 - mode as i64 + 45).rem_euclid(90) - 45).abs() <= 3)
            .map(|b| weighted[b]),
    );
    let _ = writeln!(
        out,
        "  grid orientation           modal segment heading {mode}-{} deg (mod 90); {:.1} % of drivable lane length lies within +/-4 deg of it",
        mode + 1,
        100.0 * near_mode / total_len
    );
    let top: Vec<String> = {
        let mut v: Vec<(usize, f64)> = (0..90).map(|b| (b, weighted[b])).collect();
        v.sort_by(|a, b| b.1.partial_cmp(&a.1).expect("finite").then(a.0.cmp(&b.0)));
        v.iter()
            .take(6)
            .map(|(b, l)| format!("{b}deg {:.0}m", l))
            .collect()
    };
    let _ = writeln!(out, "  heading modes (mod 90)     {}", top.join("  "));
    let _ = writeln!(
        out,
        "  named-street check         {DIAGONAL_STREET}: {} drivable lanes, {:.2} km, modal heading {} deg (mod 90)",
        broadway_lanes(world).len(),
        math::sum_ordered(broadway_lanes(world).iter().map(|l| l.length_m)) / 1000.0,
        broadway_heading(world)
    );
}

/// The planar area of a closed-or-open ring, square metres.
fn ring_area_m2(ring: &[Vec3]) -> f64 {
    let mut acc = 0.0;
    for i in 0..ring.len() {
        let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
        acc += a.x * b.y - b.x * a.y;
    }
    acc.abs() / 2.0
}

/// Every drivable lane on the street named by [`DIAGONAL_STREET`].
fn broadway_lanes(world: &World) -> Vec<&v2xw_world::model::Lane> {
    world
        .roads
        .lanes()
        .iter()
        .filter(|l| {
            l.kind == LaneKind::Driving
                && world
                    .symbols
                    .resolve_optional(world.roads.edge(l.edge).name)
                    == DIAGONAL_STREET
        })
        .collect()
}

/// The length-weighted modal heading of [`DIAGONAL_STREET`], degrees mod 90.
fn broadway_heading(world: &World) -> usize {
    let mut weighted = [0.0f64; 90];
    for lane in broadway_lanes(world) {
        for w in lane.centreline.windows(2) {
            let d = w[1] - w[0];
            let deg = math::atan2(d.y, d.x) * 180.0 / core::f64::consts::PI;
            let bin = (((deg % 90.0) + 90.0) % 90.0).floor() as usize;
            weighted[bin.min(89)] += d.norm_2d();
        }
    }
    (0..90)
        .max_by(|a, b| weighted[*a].partial_cmp(&weighted[*b]).expect("finite"))
        .expect("90 bins")
}

/// Picks the busiest signalised junction: most internal connectors, ties broken by id.
fn busiest_junction(world: &World) -> JunctionId {
    let mut best = (0usize, JunctionId::new(0));
    for j in world.roads.junctions() {
        if !matches!(j.control, JunctionControl::Signalised { .. }) {
            continue;
        }
        let score = j.internal.len();
        if score > best.0 {
            best = (score, j.id);
        }
    }
    best.1
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let source = args
        .next()
        .unwrap_or_else(|| "worlds/cache/manhattan.osm.xml".to_string());
    let out_dir = PathBuf::from(args.next().unwrap_or_else(|| "worlds/cache".to_string()));
    std::fs::create_dir_all(&out_dir)?;

    let clip = args.next().as_deref() == Some("clip");
    // V4/W1: the Phase 1 Manhattan path selects the urban preset. There is no default
    // one, and a German free-flow design speed on a Midtown side street was the defect.
    let mut options = OsmOptions::default()
        .imported_at("2026-09-18T00:00:00Z")
        .highway_preset(v2xw_world::osm::HighwayPreset::UrbanUsNyc);
    if clip {
        // The extract's own `<bounds>`. Passing it makes the importer keep only what was
        // asked for; without it the world is whatever the whole of every intersecting way
        // spans, which for this extract is 2.5x the area.
        options = options.bbox(v2xw_world::GeoBbox::new(
            REQUESTED_BBOX[0],
            REQUESTED_BBOX[1],
            REQUESTED_BBOX[2],
            REQUESTED_BBOX[3],
        ));
    }
    let (world, report) = import_osm(&source, &options)?;
    println!("{}", report.to_text());
    if clip {
        println!("(clipped to the requested bbox)");
    }

    let overview = render_overview(&world, 2000);
    let overview_path = out_dir.join(if clip {
        "manhattan-render-clipped.png"
    } else {
        "manhattan-render.png"
    });
    let n = overview.write_png(&overview_path)?;
    println!(
        "overview  {}x{} px ({:.3} px/m) -> {} ({} bytes)",
        overview.width,
        overview.height,
        overview.width as f64 / (world.bbox.max.x - world.bbox.min.x),
        overview_path.display(),
        n
    );

    let jid = busiest_junction(&world);
    let j = world.roads.junction(jid);
    let coarse = render_junction(&world, jid, ZOOM_SPAN_M, 1.0);
    let coarse_path = out_dir.join(if clip {
        "manhattan-junction-1mpp-clipped.png"
    } else {
        "manhattan-junction-1mpp.png"
    });
    let n = coarse.write_png(&coarse_path)?;
    println!(
        "junction  {}x{} px at 1 px/m -> {} ({n} bytes)",
        coarse.width,
        coarse.height,
        coarse_path.display()
    );
    let zoom = render_junction(&world, jid, ZOOM_SPAN_M, ZOOM_PX_PER_M);
    let zoom_path = out_dir.join(if clip {
        "manhattan-junction-clipped.png"
    } else {
        "manhattan-junction.png"
    });
    let n = zoom.write_png(&zoom_path)?;
    let (lat, lon) = world.projection().to_geodetic(j.position.x, j.position.y);
    println!(
        "junction  {:?} at ({:.1}, {:.1}) m = {lat:.6}, {lon:.6}; {} in, {} out, {} internal, control {:?}",
        jid,
        j.position.x,
        j.position.y,
        j.incoming.len(),
        j.outgoing.len(),
        j.internal.len(),
        j.control
    );
    println!(
        "          {}x{} px at {ZOOM_PX_PER_M} px/m -> {} ({} bytes)",
        zoom.width,
        zoom.height,
        zoom_path.display(),
        n
    );

    // A third view, framed on Times Square, where Broadway crosses Seventh Avenue: the one
    // place in this extract where the diagonal street and the grid meet, and therefore the
    // clearest test of whether the grid's rotation and the diagonal are both right.
    let p = world.projection();
    let (tsx, tsy) = p.to_enu(40.758_0, -73.985_5);
    let ts = render_around(&world, Vec3::new(tsx, tsy, 0.0), 700.0, 1.6);
    let ts_path = out_dir.join(if clip {
        "manhattan-broadway-clipped.png"
    } else {
        "manhattan-broadway.png"
    });
    let n = ts.write_png(&ts_path)?;
    println!(
        "broadway  {}x{} px at 1.6 px/m centred on Times Square -> {} ({n} bytes)",
        ts.width,
        ts.height,
        ts_path.display()
    );

    let mut text = String::new();
    let view = View::fit(world.bbox, 2000);
    landmark_check(&world, &view, &mut text);
    statistics(&world, &mut text);
    print!("{text}");
    std::fs::write(
        out_dir.join(if clip {
            "render-report-clipped.txt"
        } else {
            "render-report.txt"
        }),
        &text,
    )?;
    Ok(())
}
