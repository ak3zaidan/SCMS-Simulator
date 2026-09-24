//! The `World` data model — 03-interfaces.md §2 and 04-models.md §1.1, model id
//! `world/format/world-1`.
//!
//! One object holds everything the engine needs about static geometry: the geodetic
//! origin and its projection, the lane-level road network, buildings, terrain, signal
//! plans, infrastructure sites, land-use zones, the import provenance and the content
//! hash. 04-models.md §1.1 records why this is our own format rather than SUMO
//! `net.xml`, OpenDRIVE or Lanelet2: no external format carries buildings, terrain,
//! propagation environment classes, provenance and render hints in one object, and the
//! engine needs one object for mobility, propagation and rendering.
//!
//! # Conventions (D6)
//!
//! * Coordinates are **world-local East-North-Up metres**: `x` east, `y` north, `z` up.
//! * The origin of the plane is the world bounding box's **south-west corner**, so every
//!   `x` and `y` in a world is non-negative.
//! * Headings are radians, ENU, `0 = east`, counter-clockwise.
//! * Positions are `f64` throughout; `f32` appears only in the wire protocol
//!   ([`crate::serde_vwp`]) and the renderer.
//!
//! # Determinism
//!
//! * Every float in a world is on its quantisation grid from the moment it is built
//!   ([`crate::quant`]): [`Lane::new`] and [`WorldBuilder::build`] quantise.
//! * Every collection is a `Vec` indexed by a dense id, or a `BTreeMap`. There is no
//!   `HashMap` anywhere in the model, so no iteration order can leak into an output.
//! * Ids are dense and assigned in a documented order; see [`RoadNetwork`] and
//!   [`crate::procedural`].
//! * Every transcendental goes through [`v2xw_core::math`], never the standard library
//!   (ADR 0003).
//!
//! # Immutability (I-W1)
//!
//! A `World` is immutable after it is built, except for signal-plan *state* and dynamic
//! closures, which live outside this struct (they are engine state keyed by
//! [`v2xw_core::ids::SignalId`] and [`v2xw_core::ids::LaneId`], not fields here). The
//! spatial indices of [`crate::index`] are built lazily from the data on first query and
//! cached; [`World::reindex`] drops the cache, and the only way to reach it is `&mut
//! World`, so a shared `&World` can never observe a stale index.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use v2xw_core::geom::{Bbox, LanePos, Vec3};
use v2xw_core::ids::{BuildingId, EdgeId, JunctionId, LaneId, NodeId, SignalId};
use v2xw_core::math;

use crate::error::{Result, WorldError};
use crate::index::{IndexOptions, WorldIndex};
use crate::quant::{
    Q_DB, Q_DEGREES, Q_HEIGHT_M, Q_POSITION_M, Q_SPEED_MPS, Q_TIME_S, quantise, quantise_vec3,
};

/// Declares one dense `u32` newtype id, in the style of [`v2xw_core::ids`].
///
/// `v2xw-core` owns the ids that cross crate boundaries (`LaneId`, `EdgeId`,
/// `JunctionId`, `SignalId`, `BuildingId`, …). Crossings, sites and land-use zones are
/// addressed only by the world, its wire payload and the "why" inspector, so their ids
/// are declared here rather than by widening the core contract.
macro_rules! define_world_id {
    ($(#[$meta:meta])* $name:ident, $prefix:literal) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        #[repr(transparent)]
        pub struct $name(
            /// The dense index.
            pub u32,
        );

        impl $name {
            #[doc = concat!("Creates a `", stringify!($name), "` from a dense index.")]
            pub const fn new(index: u32) -> Self {
                Self(index)
            }

            /// The dense index.
            pub const fn index(self) -> u32 {
                self.0
            }

            /// The dense index as `usize`.
            pub const fn as_usize(self) -> usize {
                self.0 as usize
            }
        }

        impl core::fmt::Display for $name {
            #[doc = concat!("Formats as `", $prefix, "<index>`.")]
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write!(f, concat!($prefix, "{}"), self.0)
            }
        }
    };
}

define_world_id!(
    /// A pedestrian or cyclist crossing of a road.
    CrossingId,
    "x"
);
define_world_id!(
    /// A candidate infrastructure site: an RSU mast or a cell site.
    SiteId,
    "st"
);
define_world_id!(
    /// A land-use zone.
    ZoneId,
    "z"
);
define_world_id!(
    /// An interned string in a world's [`SymbolTable`].
    ///
    /// Four bytes in the model instead of a `String`: 04-models.md §1.1 wants names and
    /// tags to be cheap, because a city has one street name per hundred lanes.
    /// `SymbolId(0)` is always the empty string.
    SymbolId,
    "s"
);

// ---------------------------------------------------------------------------
// Geodesy
// ---------------------------------------------------------------------------

/// The geodetic anchor of a world's local tangent plane (D6).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GeoOrigin {
    /// Latitude of world `(0, 0)`, degrees north, WGS-84.
    pub lat_deg: f64,
    /// Longitude of world `(0, 0)`, degrees east, WGS-84.
    pub lon_deg: f64,
    /// Ellipsoidal height of world `z = 0`, metres.
    pub alt_m: f64,
}

impl GeoOrigin {
    /// Creates an origin, quantising its coordinates onto the grids of [`crate::quant`].
    pub fn new(lat_deg: f64, lon_deg: f64, alt_m: f64) -> Self {
        Self {
            lat_deg: quantise(lat_deg, Q_DEGREES),
            lon_deg: quantise(lon_deg, Q_DEGREES),
            alt_m: quantise(alt_m, Q_HEIGHT_M),
        }
    }

    /// The origin of a world with no geodetic anchor: null island at sea level.
    ///
    /// Procedural worlds have no real location. Using `(0, 0, 0)` rather than an invented
    /// city keeps the fiction visible, and the provenance records the generator instead
    /// of a source bounding box.
    pub const NULL_ISLAND: GeoOrigin = GeoOrigin {
        lat_deg: 0.0,
        lon_deg: 0.0,
        alt_m: 0.0,
    };
}

/// A geodetic bounding box, as an OSM or Overpass query gives it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GeoBbox {
    /// South edge, degrees north.
    pub min_lat_deg: f64,
    /// West edge, degrees east.
    pub min_lon_deg: f64,
    /// North edge, degrees north.
    pub max_lat_deg: f64,
    /// East edge, degrees east.
    pub max_lon_deg: f64,
}

impl GeoBbox {
    /// Creates a geodetic box, quantising onto [`crate::quant::Q_DEGREES`] and ordering
    /// the corners so that `min ≤ max`.
    pub fn new(min_lat_deg: f64, min_lon_deg: f64, max_lat_deg: f64, max_lon_deg: f64) -> Self {
        Self {
            min_lat_deg: quantise(min_lat_deg.min(max_lat_deg), Q_DEGREES),
            min_lon_deg: quantise(min_lon_deg.min(max_lon_deg), Q_DEGREES),
            max_lat_deg: quantise(min_lat_deg.max(max_lat_deg), Q_DEGREES),
            max_lon_deg: quantise(min_lon_deg.max(max_lon_deg), Q_DEGREES),
        }
    }

    /// The south-west corner, which D6 makes the local plane's origin.
    pub fn south_west(&self) -> (f64, f64) {
        (self.min_lat_deg, self.min_lon_deg)
    }

    /// The centre of the box.
    pub fn centre(&self) -> (f64, f64) {
        (
            (self.min_lat_deg + self.max_lat_deg) * 0.5,
            (self.min_lon_deg + self.max_lon_deg) * 0.5,
        )
    }

    /// `[min_lon, min_lat, max_lon, max_lat]` — the order the `vwp-world/1` provenance
    /// document and the Overpass `bbox=` parameter both use
    /// (docs/protocol/vwp-v1.md §4.6, 04-models.md §1.2).
    pub fn to_lon_lat_array(&self) -> [f64; 4] {
        [
            self.min_lon_deg,
            self.min_lat_deg,
            self.max_lon_deg,
            self.max_lat_deg,
        ]
    }
}

/// The local tangent plane: geodetic coordinates to world-local ENU metres and back.
///
/// # The approximation
///
/// This is an **equirectangular (plate carrée) projection about the origin**: a
/// longitude difference is multiplied by the metres-per-degree-of-longitude *at the
/// origin's latitude*, and a latitude difference by the metres per degree of latitude
/// there. The two scale factors are the standard truncated series for the WGS-84
/// ellipsoid,
///
/// ```text
/// m_lat(φ) = 111_132.92 − 559.82·cos 2φ + 1.175·cos 4φ
/// m_lon(φ) = 111_412.84·cos φ − 93.5·cos 3φ
/// ```
///
/// which are the meridian- and parallel-arc expansions with their third terms dropped.
/// Dropping them costs at most 0.0023 m per degree of latitude and 0.118 m per degree of
/// longitude, i.e. about 1.4 mm per kilometre east — three orders of magnitude below the
/// projection's own error, computed below.
///
/// # The error, measured
///
/// Because the scale factors are frozen at the origin's latitude, a point's east
/// coordinate is wrong by roughly `Δλ · (dm_lon/dφ) · Δφ`, which grows as the *square*
/// of the extent. Measured against the Vincenty inverse solution on the WGS-84 ellipsoid
/// (the `projection_error_matches_the_documented_bound` test recomputes this):
///
/// | Square extent from the origin | Worst planar error |
/// |---|---|
/// | 1 km | 0.15 m |
/// | 2 km | 0.61 m |
/// | 4 km | 2.4 m |
/// | 10 km | 15 m |
///
/// Over the D7 Phase 1 Manhattan box (1.86 km × 2.00 km, origin at its south-west
/// corner) the worst case is **0.56 m, at the far north-east corner**.
///
/// That is acceptable for this simulator and it is deliberately not hidden: a 0.5 m
/// geodetic offset does not change a path loss, a headway or a certificate, and the
/// projection is recorded in [`WorldProvenance::projection`] so a consumer that needs
/// survey accuracy knows exactly what was applied. Determinism is unaffected — the error
/// is a smooth, deterministic function of position, and both trig calls go through
/// [`v2xw_core::math`], so it is the same on every platform, bit for bit. A future
/// transverse Mercator or a proper local ENU rotation would be a new projection id in the
/// provenance, not a silent change.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(from = "GeoOrigin", into = "GeoOrigin")]
pub struct Projection {
    origin: GeoOrigin,
    m_per_deg_lat: f64,
    m_per_deg_lon: f64,
}

impl Projection {
    /// The projection's id, as recorded in [`WorldProvenance::projection`].
    pub const NAME: &'static str = "equirectangular-local-tangent-plane/1";

    /// Builds the projection for `origin`, evaluating both scale factors once.
    pub fn new(origin: GeoOrigin) -> Self {
        let phi = origin.lat_deg.to_radians();
        let m_per_deg_lat =
            111_132.92 - 559.82 * math::cos(2.0 * phi) + 1.175 * math::cos(4.0 * phi);
        let m_per_deg_lon = 111_412.84 * math::cos(phi) - 93.5 * math::cos(3.0 * phi);
        Self {
            origin,
            m_per_deg_lat,
            m_per_deg_lon,
        }
    }

    /// The geodetic origin.
    pub fn origin(&self) -> GeoOrigin {
        self.origin
    }

    /// Metres per degree of latitude at the origin.
    pub fn metres_per_degree_latitude(&self) -> f64 {
        self.m_per_deg_lat
    }

    /// Metres per degree of longitude at the origin.
    pub fn metres_per_degree_longitude(&self) -> f64 {
        self.m_per_deg_lon
    }

    /// Projects geodetic degrees to world-local `(east, north)` metres.
    ///
    /// The result is **not** quantised: an importer projects hundreds of thousands of
    /// nodes and then simplifies the polylines, so quantising here would be wasted work
    /// and would round twice. [`Lane::new`] and [`WorldBuilder::build`] quantise once,
    /// at the end.
    pub fn to_enu(&self, lat_deg: f64, lon_deg: f64) -> (f64, f64) {
        (
            (lon_deg - self.origin.lon_deg) * self.m_per_deg_lon,
            (lat_deg - self.origin.lat_deg) * self.m_per_deg_lat,
        )
    }

    /// Projects geodetic degrees and an ellipsoidal height to a world-local point.
    pub fn to_enu_vec3(&self, lat_deg: f64, lon_deg: f64, alt_m: f64) -> Vec3 {
        let (x, y) = self.to_enu(lat_deg, lon_deg);
        Vec3::new(x, y, alt_m - self.origin.alt_m)
    }

    /// The exact inverse of [`Projection::to_enu`]: world-local metres back to
    /// `(latitude, longitude)` degrees.
    ///
    /// Exact in the sense that the round trip reproduces the input to within
    /// floating-point rounding — the same approximation is applied in reverse, so the
    /// projection error above does not accumulate over a round trip.
    pub fn to_geodetic(&self, x_m: f64, y_m: f64) -> (f64, f64) {
        (
            self.origin.lat_deg + y_m / self.m_per_deg_lat,
            self.origin.lon_deg + x_m / self.m_per_deg_lon,
        )
    }

    /// The ellipsoidal height of a world-local `z`.
    pub fn to_altitude(&self, z_m: f64) -> f64 {
        self.origin.alt_m + z_m
    }
}

/// The core crate's [`v2xw_core::GeoOrigin`] is the canonical definition of this type and
/// this one is a duplicate of it, field for field, with the same projection and the same
/// quantisation grid. Until this becomes a re-export the two must not diverge, so the
/// conversions are total and lossless in both directions and there is no constructor
/// between them that could quietly reinterpret a field.
///
/// The duplication is real technical debt and it has already cost once: it produced a
/// type error at the engine's world-to-node boundary that no reader had noticed, because
/// the two names are identical and only the crate path differs.
impl From<GeoOrigin> for v2xw_core::GeoOrigin {
    fn from(o: GeoOrigin) -> Self {
        Self {
            lat_deg: o.lat_deg,
            lon_deg: o.lon_deg,
            alt_m: o.alt_m,
        }
    }
}

impl From<v2xw_core::GeoOrigin> for GeoOrigin {
    fn from(o: v2xw_core::GeoOrigin) -> Self {
        Self {
            lat_deg: o.lat_deg,
            lon_deg: o.lon_deg,
            alt_m: o.alt_m,
        }
    }
}

impl From<GeoOrigin> for Projection {
    fn from(origin: GeoOrigin) -> Self {
        Projection::new(origin)
    }
}

impl From<Projection> for GeoOrigin {
    fn from(p: Projection) -> Self {
        p.origin
    }
}

// ---------------------------------------------------------------------------
// String interning
// ---------------------------------------------------------------------------

/// A world's string table: names and tag values, interned to a [`SymbolId`].
///
/// Ids are assigned in **insertion order**, which is deterministic because every caller
/// (importer or generator) interns in a documented order. Id `0` is the empty string, as
/// the wire symbol table also requires (docs/protocol/vwp-v1.md §2.5), so
/// `Option<SymbolId>::None` and `SymbolId(0)` both render as `""`.
///
/// The lookup side is a `BTreeMap`, not a `HashMap`: this crate has no hash iteration
/// anywhere, so no accidental ordering dependency can ever creep in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "Vec<String>", into = "Vec<String>")]
pub struct SymbolTable {
    strings: Vec<String>,
    index: BTreeMap<String, u32>,
}

impl SymbolTable {
    /// A table holding only the empty string at id `0`.
    pub fn new() -> Self {
        Self {
            strings: vec![String::new()],
            index: BTreeMap::from([(String::new(), 0)]),
        }
    }

    /// Interns `s`, returning its existing id or appending it.
    pub fn intern(&mut self, s: &str) -> SymbolId {
        if let Some(&id) = self.index.get(s) {
            return SymbolId::new(id);
        }
        let id = u32::try_from(self.strings.len()).expect("a world cannot hold 2^32 strings");
        self.strings.push(s.to_string());
        self.index.insert(s.to_string(), id);
        SymbolId::new(id)
    }

    /// Interns `s` and wraps it as `Some`, or returns `None` for the empty string.
    ///
    /// The model spells "no name" as `None` rather than `Some(SymbolId(0))`, so that
    /// `Option::is_none` is the only test a consumer needs.
    pub fn intern_optional(&mut self, s: &str) -> Option<SymbolId> {
        if s.is_empty() {
            None
        } else {
            Some(self.intern(s))
        }
    }

    /// The string behind `id`, or `""` if the id is not in this table.
    ///
    /// Returning `""` rather than panicking is deliberate: a name is decoration, and a
    /// world whose symbol table lost an entry should still render.
    pub fn resolve(&self, id: SymbolId) -> &str {
        self.strings.get(id.as_usize()).map_or("", String::as_str)
    }

    /// The string behind an optional id, or `""`.
    pub fn resolve_optional(&self, id: Option<SymbolId>) -> &str {
        id.map_or("", |i| self.resolve(i))
    }

    /// Looks up an id without interning.
    pub fn get(&self, s: &str) -> Option<SymbolId> {
        self.index.get(s).copied().map(SymbolId::new)
    }

    /// Every string, in id order. `strings()[0]` is always `""`.
    pub fn strings(&self) -> &[String] {
        &self.strings
    }

    /// How many strings the table holds, including the empty string.
    pub fn len(&self) -> usize {
        self.strings.len()
    }

    /// True if the table holds nothing but the empty string.
    pub fn is_empty(&self) -> bool {
        self.strings.len() <= 1
    }
}

impl Default for SymbolTable {
    fn default() -> Self {
        Self::new()
    }
}

impl From<Vec<String>> for SymbolTable {
    /// Rebuilds a table from its strings, repairing a missing or misplaced empty string
    /// at id 0 so that a hand-edited debug JSON still loads.
    fn from(mut strings: Vec<String>) -> Self {
        if strings.first().map(String::as_str) != Some("") {
            strings.insert(0, String::new());
        }
        let mut index = BTreeMap::new();
        for (i, s) in strings.iter().enumerate() {
            index.entry(s.clone()).or_insert(i as u32);
        }
        Self { strings, index }
    }
}

impl From<SymbolTable> for Vec<String> {
    fn from(t: SymbolTable) -> Self {
        t.strings
    }
}

/// Serialises a 32-byte hash as lower-case hex, so a debug JSON shows
/// `"content_hash": "d172…"` rather than 32 integers.
mod hex32 {
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

    pub fn serialize<S: Serializer>(bytes: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v2xw_core::hash::hex_encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let text = String::deserialize(d)?;
        if text.len() != 64 {
            return Err(D::Error::custom(format!(
                "a 32-byte hash is 64 hex characters, got {}",
                text.len()
            )));
        }
        let mut out = [0u8; 32];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).map_err(D::Error::custom)?;
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Road network
// ---------------------------------------------------------------------------

/// What a lane is for.
///
/// The discriminants are the `lane_type` codes of docs/protocol/vwp-v1.md §4.3 and
/// Appendix A, so [`LaneKind::wire_code`] is a cast and the two can never drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[repr(u8)]
pub enum LaneKind {
    /// A general traffic lane.
    Driving = 0,
    /// A cycle lane or track.
    Cycle = 1,
    /// A footway alongside a road.
    Sidewalk = 2,
    /// A bus-only lane.
    Bus = 3,
    /// A parking lane.
    Parking = 4,
    /// A connector inside a junction ([`Junction::internal`]).
    Internal = 5,
    /// A marked crossing carried as a lane, for pedestrian routing.
    Crossing = 6,
}

impl LaneKind {
    /// Every kind, in wire-code order.
    pub const ALL: [LaneKind; 7] = [
        LaneKind::Driving,
        LaneKind::Cycle,
        LaneKind::Sidewalk,
        LaneKind::Bus,
        LaneKind::Parking,
        LaneKind::Internal,
        LaneKind::Crossing,
    ];

    /// The `lane_type` code of docs/protocol/vwp-v1.md §4.3.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }

    /// The kind for a wire code, or `None` if the code is not one of the seven.
    pub const fn from_wire_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(LaneKind::Driving),
            1 => Some(LaneKind::Cycle),
            2 => Some(LaneKind::Sidewalk),
            3 => Some(LaneKind::Bus),
            4 => Some(LaneKind::Parking),
            5 => Some(LaneKind::Internal),
            6 => Some(LaneKind::Crossing),
            _ => None,
        }
    }

    /// The spelling the `vwp-world/1` JSON form uses (§4.6).
    pub const fn wire_name(self) -> &'static str {
        match self {
            LaneKind::Driving => "drive",
            LaneKind::Cycle => "bike",
            LaneKind::Sidewalk => "sidewalk",
            LaneKind::Bus => "bus",
            LaneKind::Parking => "parking",
            LaneKind::Internal => "junction-internal",
            LaneKind::Crossing => "crossing",
        }
    }

    /// True if a motor vehicle may normally drive here.
    pub const fn is_motorised(self) -> bool {
        matches!(
            self,
            LaneKind::Driving | LaneKind::Bus | LaneKind::Internal | LaneKind::Parking
        )
    }
}

/// Which vehicle classes may use a lane, as a bitmask.
///
/// The bit numbering is the `allowed_classes` mask of docs/protocol/vwp-v1.md §4.3:
/// `1` car, `2` truck, `4` bus, `8` moto, `16` bicycle, `32` pedestrian, `64` emergency,
/// `128` rail.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct ClassMask(u16);

impl ClassMask {
    /// Passenger car.
    pub const CAR: ClassMask = ClassMask(1 << 0);
    /// Goods vehicle.
    pub const TRUCK: ClassMask = ClassMask(1 << 1);
    /// Bus or coach.
    pub const BUS: ClassMask = ClassMask(1 << 2);
    /// Motorcycle or moped.
    pub const MOTO: ClassMask = ClassMask(1 << 3);
    /// Bicycle.
    pub const BICYCLE: ClassMask = ClassMask(1 << 4);
    /// Pedestrian.
    pub const PEDESTRIAN: ClassMask = ClassMask(1 << 5);
    /// Emergency vehicle.
    pub const EMERGENCY: ClassMask = ClassMask(1 << 6);
    /// Tram or train.
    pub const RAIL: ClassMask = ClassMask(1 << 7);

    /// No class at all — a closed lane.
    pub const NONE: ClassMask = ClassMask(0);
    /// Every class.
    pub const ALL: ClassMask = ClassMask(0xFF);
    /// The classes that normally use a driving lane: car, truck, bus, moto, emergency.
    pub const MOTOR_TRAFFIC: ClassMask = ClassMask(
        ClassMask::CAR.0
            | ClassMask::TRUCK.0
            | ClassMask::BUS.0
            | ClassMask::MOTO.0
            | ClassMask::EMERGENCY.0,
    );

    /// The class names in bit order, as the `vwp-world/1` JSON form spells them (§4.6).
    pub const NAMES: [&'static str; 8] = [
        "car",
        "truck",
        "bus",
        "moto",
        "bicycle",
        "pedestrian",
        "emergency",
        "rail",
    ];

    /// Wraps a raw mask. Bits above `rail` are reserved and are dropped.
    pub const fn from_bits(bits: u16) -> Self {
        ClassMask(bits & 0xFF)
    }

    /// The raw mask, for the wire format.
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// The union of two masks.
    pub const fn union(self, other: ClassMask) -> ClassMask {
        ClassMask(self.0 | other.0)
    }

    /// The intersection of two masks.
    pub const fn intersection(self, other: ClassMask) -> ClassMask {
        ClassMask(self.0 & other.0)
    }

    /// `self` without the classes in `other`.
    pub const fn difference(self, other: ClassMask) -> ClassMask {
        ClassMask(self.0 & !other.0)
    }

    /// True if no class at all is allowed.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// True if every class in `other` is allowed.
    pub const fn contains_all(self, other: ClassMask) -> bool {
        self.0 & other.0 == other.0
    }

    /// True if at least one class in `other` is allowed.
    pub const fn contains_any(self, other: ClassMask) -> bool {
        self.0 & other.0 != 0
    }

    /// The allowed class names, in bit order.
    pub fn names(self) -> Vec<&'static str> {
        (0..8)
            .filter(|b| self.0 & (1 << b) != 0)
            .map(|b| ClassMask::NAMES[b as usize])
            .collect()
    }

    /// The mask for a list of class names; unknown names are ignored.
    pub fn from_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Self {
        let mut bits = 0u16;
        for n in names {
            if let Some(b) = ClassMask::NAMES.iter().position(|c| *c == n) {
                bits |= 1 << b;
            }
        }
        ClassMask(bits)
    }
}

impl core::fmt::Display for ClassMask {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.is_empty() {
            return f.write_str("none");
        }
        f.write_str(&self.names().join("+"))
    }
}

/// The functional class of a road, in the OSM `highway=*` sense.
///
/// The importer maps `highway` tags onto this; the mobility and propagation models use it
/// for default speeds, default lane counts and environment presets (04-models.md §1.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RoadClass {
    /// Motorway or freeway.
    Motorway,
    /// Trunk road.
    Trunk,
    /// Primary road.
    Primary,
    /// Secondary road.
    Secondary,
    /// Tertiary road.
    Tertiary,
    /// Residential street.
    Residential,
    /// Living street or shared space.
    Living,
    /// Service road, alley or driveway.
    Service,
    /// Motorway or trunk slip road.
    Link,
    /// Footway or pedestrian street.
    Footway,
    /// Cycleway.
    Cycleway,
    /// Track or path.
    Path,
    /// The synthetic edge that owns a junction's internal lanes.
    Internal,
    /// Anything the importer could not classify.
    Unclassified,
}

impl RoadClass {
    /// A stable lower-case label, used by the content hash and the import report.
    ///
    /// Spelled out rather than derived from `Debug`, so that renaming a variant is a
    /// deliberate change to the hash rather than an accident.
    pub const fn label(self) -> &'static str {
        match self {
            RoadClass::Motorway => "motorway",
            RoadClass::Trunk => "trunk",
            RoadClass::Primary => "primary",
            RoadClass::Secondary => "secondary",
            RoadClass::Tertiary => "tertiary",
            RoadClass::Residential => "residential",
            RoadClass::Living => "living",
            RoadClass::Service => "service",
            RoadClass::Link => "link",
            RoadClass::Footway => "footway",
            RoadClass::Cycleway => "cycleway",
            RoadClass::Path => "path",
            RoadClass::Internal => "internal",
            RoadClass::Unclassified => "unclassified",
        }
    }
}

/// Which way a movement turns, as seen by the driver.
///
/// The names follow SUMO's `<connection dir>` codes, because the SUMO importer of
/// 04-models.md §1.2 maps straight onto them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TurnDirection {
    /// Straight on.
    Straight,
    /// A left turn (SUMO `l`).
    Left,
    /// A right turn (SUMO `r`).
    Right,
    /// A slight left (SUMO `L`).
    SlightLeft,
    /// A slight right (SUMO `R`).
    SlightRight,
    /// A U-turn (SUMO `t`).
    UTurn,
}

impl TurnDirection {
    /// The turn a heading change of `delta_rad` (radians, counter-clockwise positive)
    /// represents.
    ///
    /// Thresholds: within ±22.5° is straight, ±22.5–67.5° is slight, ±67.5–157.5° is a
    /// turn, beyond ±157.5° is a U-turn. These are the usual eighth-of-a-circle bands;
    /// they are geometry bookkeeping, not a calibrated model, and they are applied to a
    /// heading difference that is itself derived from quantised coordinates, so two
    /// engines agree on the classification (D10).
    pub fn from_heading_change(delta_rad: f64) -> Self {
        let d = normalise_angle(delta_rad);
        const EIGHTH: f64 = core::f64::consts::FRAC_PI_8;
        if d.abs() <= EIGHTH {
            TurnDirection::Straight
        } else if d.abs() <= 3.0 * EIGHTH {
            if d > 0.0 {
                TurnDirection::SlightLeft
            } else {
                TurnDirection::SlightRight
            }
        } else if d.abs() <= 7.0 * EIGHTH {
            if d > 0.0 {
                TurnDirection::Left
            } else {
                TurnDirection::Right
            }
        } else {
            TurnDirection::UTurn
        }
    }

    /// A stable lower-case label, used by the content hash and the wire forms.
    pub const fn label(self) -> &'static str {
        match self {
            TurnDirection::Straight => "straight",
            TurnDirection::Left => "left",
            TurnDirection::Right => "right",
            TurnDirection::SlightLeft => "slight-left",
            TurnDirection::SlightRight => "slight-right",
            TurnDirection::UTurn => "u-turn",
        }
    }

    /// The SUMO `<connection dir>` code.
    pub const fn sumo_code(self) -> char {
        match self {
            TurnDirection::Straight => 's',
            TurnDirection::Left => 'l',
            TurnDirection::Right => 'r',
            TurnDirection::SlightLeft => 'L',
            TurnDirection::SlightRight => 'R',
            TurnDirection::UTurn => 't',
        }
    }

    /// True if the movement crosses opposing traffic in right-hand traffic.
    pub const fn crosses_opposing_traffic(self) -> bool {
        matches!(self, TurnDirection::Left | TurnDirection::UTurn)
    }
}

/// Wraps an angle into `(-π, π]`.
///
/// Arithmetic only — no transcendental — so it is exact on every platform, and it
/// **terminates for every input**. The subtract-in-a-loop form this replaced did not: at
/// `a = f64::MAX` the subtraction of `2π` is absorbed and the loop never exits, and at
/// `a = 1e9` it needs more than ten million iterations. Both are reachable from outside
/// the crate, through this function and through [`TurnDirection::from_heading_change`],
/// neither of which documents a bound on its argument.
///
/// # Why a remainder and not a scaled round
///
/// `a - τ · (a / τ).round()` also terminates, but for a huge `a` the product `τ · k` is
/// rounded, and subtracting it leaves a value that is not in `(-π, π]` at all. `%` is
/// IEEE-754 `fmod`: its result `a − n·τ` is **exactly representable and exactly
/// computed** — a remainder is one of the few floating-point operations that never
/// rounds — so every platform returns the same bits and the result is always inside one
/// full turn of zero. It is not a transcendental and needs no `v2xw_core::math` routing
/// (ADR 0003 is about functions whose results are *approximations*, which is what makes
/// two libms disagree).
///
/// Over the range every caller uses — a difference of two `heading_2d()` results, so
/// `|a| < 2π`, and in fact anywhere in `|a| ≤ 2τ` — this returns **the same bits** as the
/// loop it replaces: both do a single exact addition or subtraction of `τ` there
/// (Sterbenz's lemma makes it exact), so no geometry moves. Beyond `2τ` the loop
/// accumulated a rounding error per turn and this does not, so where the two differ this
/// one is the more accurate.
///
/// `NaN` passes through, as it always did. `±∞` has no angle, and returns `NaN`.
///
/// ```
/// use v2xw_world::model::normalise_angle;
/// let pi = core::f64::consts::PI;
/// assert_eq!(normalise_angle(0.5), 0.5);
/// assert_eq!(normalise_angle(pi), pi);
/// assert_eq!(normalise_angle(-pi), pi);
/// // Terminates, and lands in range, for an argument the loop form never returned from.
/// assert!(normalise_angle(1e9) > -pi && normalise_angle(1e9) <= pi);
/// assert!(normalise_angle(f64::MAX).abs() <= pi);
/// ```
pub fn normalise_angle(a: f64) -> f64 {
    const PI: f64 = core::f64::consts::PI;
    const TAU: f64 = core::f64::consts::TAU;
    if a.is_nan() {
        return a;
    }
    if a <= PI && a > -PI {
        return a;
    }
    // Exact: `%` on `f64` is `fmod`, whose result is representable without rounding.
    let r = a % TAU;
    if r == 0.0 {
        // `fmod` keeps the sign of its left operand, so a whole number of turns
        // *backwards* reduces to `-0.0`, where the loop form's `a += τ` produced `+0.0`.
        // Nothing downstream can see the difference (`grid_index` maps both to 0), but
        // "the same bits as before" is a claim worth keeping literally true.
        return 0.0;
    }
    if r > PI {
        r - TAU
    } else if r <= -PI {
        r + TAU
    } else {
        r
    }
}

/// One lane: a centreline polyline plus its attributes.
///
/// Geometry is quantised by [`Lane::new`], which is the only constructor, so a lane's
/// coordinates are on the [`crate::quant::Q_POSITION_M`] grid from birth and
/// [`Lane::length_m`] and [`Lane::cumulative`] are on it too.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lane {
    /// This lane's dense id.
    pub id: LaneId,
    /// The edge that owns it. A junction's internal lanes are owned by the synthetic
    /// edge of [`RoadClass::Internal`] that the importer or generator creates for that
    /// junction, so this is never absent.
    pub edge: EdgeId,
    /// The junction this lane is internal to, for [`LaneKind::Internal`] lanes.
    ///
    /// Present exactly when the lane is internal; it is the `junction_id` column of
    /// docs/protocol/vwp-v1.md §4.3, which is `0xFFFFFFFF` for ordinary lanes.
    pub junction: Option<JunctionId>,
    /// Index within the edge, `0` = rightmost in the direction of travel (§4.3).
    pub index: u8,
    /// What the lane is for.
    pub kind: LaneKind,
    /// The centreline, in travel order, world-local ENU metres. At least two points, and
    /// successive points at least 1 mm apart.
    pub centreline: Vec<Vec3>,
    /// Lane width, metres.
    pub width_m: f64,
    /// Speed limit, m/s.
    pub speed_limit_mps: f64,
    /// Which classes may use it.
    pub allowed: ClassMask,
    /// Total centreline length, metres — `cumulative` last entry.
    pub length_m: f64,
    /// Arc length from the start to each centreline point, metres.
    ///
    /// `cumulative.len() == centreline.len()`, `cumulative[0] == 0.0`, non-decreasing,
    /// and strictly increasing because [`Lane::new`] rejects segments under 1 mm. Each
    /// entry is quantised, so it matches the sum of the (quantised) segment lengths to
    /// within half a millimetre per entry rather than exactly.
    pub cumulative: Vec<f64>,
}

/// Where a point falls on a lane: the result of [`Lane::project_point`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LaneProjection {
    /// Arc length of the closest point along the centreline, metres.
    pub s_m: f64,
    /// Lateral offset, metres, positive to the left of travel.
    pub d_m: f64,
    /// Horizontal distance from the query point to the centreline, metres.
    ///
    /// This is `|d_m|`; it is kept separate because a caller comparing candidate lanes
    /// wants a magnitude and a caller placing a vehicle wants the sign.
    pub distance_m: f64,
    /// The closest point on the centreline itself.
    pub point: Vec3,
    /// The index of the centreline segment the closest point lies on.
    pub segment: usize,
}

impl Lane {
    /// Builds a lane, quantising its geometry and computing its arc-length table.
    ///
    /// # Errors
    ///
    /// * [`WorldError::ShortCentreline`] if fewer than two points are given;
    /// * [`WorldError::NonFinite`] if a coordinate is not finite;
    /// * [`WorldError::DegenerateSegment`] if two successive points are closer than 1 mm
    ///   *after quantisation* — the wire format forbids it (vwp-v1 §4.3) and a
    ///   zero-length segment has no heading.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: LaneId,
        edge: EdgeId,
        junction: Option<JunctionId>,
        index: u8,
        kind: LaneKind,
        centreline: impl IntoIterator<Item = Vec3>,
        width_m: f64,
        speed_limit_mps: f64,
        allowed: ClassMask,
    ) -> Result<Self> {
        let points: Vec<Vec3> = centreline.into_iter().map(quantise_vec3).collect();
        if points.len() < 2 {
            return Err(WorldError::ShortCentreline {
                lane: id,
                points: points.len(),
            });
        }
        for p in &points {
            if !p.is_finite() {
                return Err(WorldError::NonFinite {
                    what: format!("lane {id} centreline"),
                });
            }
        }
        let mut cumulative = Vec::with_capacity(points.len());
        cumulative.push(0.0);
        let mut running = 0.0;
        for (i, pair) in points.windows(2).enumerate() {
            let step = pair[0].distance(pair[1]);
            if step < Q_POSITION_M {
                return Err(WorldError::DegenerateSegment {
                    lane: id,
                    index: i,
                    next: i + 1,
                    distance_m: step,
                });
            }
            running += step;
            cumulative.push(quantise(running, Q_POSITION_M));
        }
        let length_m = *cumulative.last().expect("pushed at least one entry");
        Ok(Self {
            id,
            edge,
            junction,
            index,
            kind,
            centreline: points,
            width_m: quantise(width_m, Q_POSITION_M),
            speed_limit_mps: quantise(speed_limit_mps, Q_SPEED_MPS),
            allowed,
            length_m,
            cumulative,
        })
    }

    /// The lane's first centreline point.
    pub fn start(&self) -> Vec3 {
        self.centreline[0]
    }

    /// The lane's last centreline point.
    pub fn end(&self) -> Vec3 {
        *self
            .centreline
            .last()
            .expect("a lane always has two or more points")
    }

    /// How many centreline points it has.
    pub fn point_count(&self) -> usize {
        self.centreline.len()
    }

    /// The `i`-th segment as `(from, to)`.
    ///
    /// # Panics
    ///
    /// If `i >= point_count() - 1`.
    pub fn segment(&self, i: usize) -> (Vec3, Vec3) {
        (self.centreline[i], self.centreline[i + 1])
    }

    /// The segment index containing arc length `s_m`, clamped to the lane.
    ///
    /// Binary search over [`Lane::cumulative`], so this is `O(log n)` and its result is a
    /// pure function of the table. A point exactly on a vertex belongs to the segment
    /// that *starts* there, so a query at `s = 0` uses the first segment.
    pub fn segment_at(&self, s_m: f64) -> usize {
        let last = self.centreline.len() - 2;
        if s_m <= 0.0 || s_m.is_nan() {
            return 0;
        }
        match self
            .cumulative
            .binary_search_by(|c| c.partial_cmp(&s_m).expect("arc lengths are finite"))
        {
            Ok(i) => i.min(last),
            Err(i) => i.saturating_sub(1).min(last),
        }
    }

    /// The centreline point at arc length `s_m`, clamped to `[0, length_m]`.
    pub fn point_at(&self, s_m: f64) -> Vec3 {
        let i = self.segment_at(s_m);
        let (a, b) = self.segment(i);
        let seg_len = self.cumulative[i + 1] - self.cumulative[i];
        if seg_len <= 0.0 {
            return a;
        }
        let t = ((s_m - self.cumulative[i]) / seg_len).clamp(0.0, 1.0);
        a.lerp(b, t)
    }

    /// The heading of the lane at arc length `s_m`: radians, ENU, `0 = east`,
    /// counter-clockwise.
    pub fn heading_at(&self, s_m: f64) -> f64 {
        let i = self.segment_at(s_m);
        let (a, b) = self.segment(i);
        (b - a).heading_2d()
    }

    /// Position and heading at arc length `s_m`.
    pub fn pose_at(&self, s_m: f64) -> (Vec3, f64) {
        (self.point_at(s_m), self.heading_at(s_m))
    }

    /// The point at arc length `s_m` offset `d_m` to the left of travel.
    ///
    /// This is the geometry behind [`World::to_xyz`]. The offset direction is the
    /// horizontal left normal of the segment at `s_m`, so a point on a curve is offset
    /// along the segment's normal rather than along a mitred corner — the same rule the
    /// mobility models use when they place a vehicle at a lateral offset.
    pub fn offset_point(&self, s_m: f64, d_m: f64) -> Vec3 {
        if d_m == 0.0 {
            return self.point_at(s_m);
        }
        let i = self.segment_at(s_m);
        let (a, b) = self.segment(i);
        let dir = (b - a).normalized();
        let left = Vec3::new(-dir.y, dir.x, 0.0);
        self.point_at(s_m) + left.scale(d_m)
    }

    /// The closest point on the centreline to `p`, in the horizontal plane.
    ///
    /// Horizontal on purpose: a lane's `z` is the road surface, and a query point is a
    /// vehicle reference point or a map click, both of which should snap to the lane
    /// under them rather than to a lane on a bridge above. The returned `point` carries
    /// the interpolated lane `z`, so a caller that wants the surface height gets it.
    ///
    /// Ties between segments go to the **later** segment, which is the one
    /// [`Lane::segment_at`] resolves that arc length to, so that
    /// `offset_point(s, d)` of the result reproduces the query point. The comparison is
    /// `<=` on `f64` with no tolerance, so a tie is a bit-identical distance and the
    /// choice is reproducible on every platform.
    pub fn project_point(&self, p: Vec3) -> LaneProjection {
        let mut best = self.project_on_segment(0, p);
        for i in 1..self.centreline.len() - 1 {
            let candidate = self.project_on_segment(i, p);
            if candidate.distance_m <= best.distance_m {
                best = candidate;
            }
        }
        best
    }

    /// The closest point on one centreline segment to `p`, in the horizontal plane.
    ///
    /// The spatial index projects onto the individual segments its grid cells hold,
    /// rather than onto whole lanes, so this is the primitive both paths share and both
    /// therefore give bit-identical answers.
    ///
    /// # Panics
    ///
    /// If `i >= point_count() - 1`.
    pub fn project_on_segment(&self, i: usize, p: Vec3) -> LaneProjection {
        let (a, b) = self.segment(i);
        let ab = b - a;
        let len2 = ab.x * ab.x + ab.y * ab.y;
        let t = if len2 <= 0.0 {
            0.0
        } else {
            (((p.x - a.x) * ab.x + (p.y - a.y) * ab.y) / len2).clamp(0.0, 1.0)
        };
        let closest = a.lerp(b, t);
        let dx = p.x - closest.x;
        let dy = p.y - closest.y;
        let dir = ab.normalized();
        LaneProjection {
            s_m: self.cumulative[i] + (self.cumulative[i + 1] - self.cumulative[i]) * t,
            // Positive to the left of travel: the left normal of (dx, dy) is (-dy, dx).
            d_m: dx * -dir.y + dy * dir.x,
            distance_m: math::sqrt(dx * dx + dy * dy),
            point: closest,
            segment: i,
        }
    }

    /// True if `classes` may use this lane (any class in common).
    pub fn admits(&self, classes: ClassMask) -> bool {
        self.allowed.contains_any(classes)
    }

    /// Re-derives the arc-length table from the centreline, for validation.
    ///
    /// Returns the raw (unquantised) cumulative lengths, which is what
    /// [`World::validate`] compares the stored table against.
    pub fn recompute_cumulative(&self) -> Vec<f64> {
        let mut out = Vec::with_capacity(self.centreline.len());
        out.push(0.0);
        let mut running = 0.0;
        for pair in self.centreline.windows(2) {
            running += pair[0].distance(pair[1]);
            out.push(running);
        }
        out
    }
}

/// A road edge: the bundle of lanes between two junctions, in one direction of travel.
///
/// A two-way street is two edges. An edge whose `from` and `to` are the same junction is
/// that junction's synthetic internal edge, which owns its connectors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    /// This edge's dense id.
    pub id: EdgeId,
    /// The junction it leaves.
    pub from: JunctionId,
    /// The junction it reaches.
    pub to: JunctionId,
    /// Its lanes, ordered by [`Lane::index`], `0` = rightmost.
    pub lanes: Vec<LaneId>,
    /// Street name, interned in the world's [`SymbolTable`].
    pub name: Option<SymbolId>,
    /// Functional class.
    pub road_class: RoadClass,
}

impl Edge {
    /// True if this is a junction's synthetic internal edge.
    pub fn is_internal(&self) -> bool {
        self.road_class == RoadClass::Internal
    }
}

/// How a junction is controlled.
///
/// The discriminants are the `control` codes of docs/protocol/vwp-v1.md §4.5 and
/// Appendix A: `0` none, `1` priority, `2` signal, `3` stop, `4` yield, `5` roundabout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JunctionControl {
    /// No control at all: whoever arrives first goes.
    Uncontrolled,
    /// Priority road: the conflict matrix says who yields.
    Priority,
    /// Traffic signals, running the named plan.
    Signalised {
        /// The signal plan controlling this junction.
        plan: SignalId,
    },
    /// Stop sign on the minor approaches.
    Stop,
    /// Give-way sign on the minor approaches.
    Yield,
    /// A roundabout.
    Roundabout,
}

impl JunctionControl {
    /// The `control` code of docs/protocol/vwp-v1.md §4.5.
    pub const fn wire_code(self) -> u8 {
        match self {
            JunctionControl::Uncontrolled => 0,
            JunctionControl::Priority => 1,
            JunctionControl::Signalised { .. } => 2,
            JunctionControl::Stop => 3,
            JunctionControl::Yield => 4,
            JunctionControl::Roundabout => 5,
        }
    }

    /// The spelling the `vwp-world/1` JSON form uses (§4.6).
    pub const fn wire_name(self) -> &'static str {
        match self {
            JunctionControl::Uncontrolled => "none",
            JunctionControl::Priority => "priority",
            JunctionControl::Signalised { .. } => "signal",
            JunctionControl::Stop => "stop",
            JunctionControl::Yield => "yield",
            JunctionControl::Roundabout => "roundabout",
        }
    }

    /// The plan id, for a signalised junction.
    pub const fn plan(self) -> Option<SignalId> {
        match self {
            JunctionControl::Signalised { plan } => Some(plan),
            _ => None,
        }
    }
}

/// Which movements through a junction conflict, and which of a conflicting pair yields.
///
/// Row and column `i` is `junction.internal[i]`: every movement through a junction owns
/// exactly one internal lane, so the internal-lane list *is* the movement list, in the
/// order the importer or generator created it. Two square bit matrices are stored:
///
/// * `foes` — symmetric: the two movements' paths cross, merge or diverge into the same
///   lane, so they cannot be used at the same time by two vehicles that would meet.
/// * `response` — `must_yield(a, b)` is true when `a` has to give way to `b`. It is
///   antisymmetric on conflicting pairs and false everywhere else. A signal plan
///   overrides it while the signal is green, exactly as SUMO's `<request>` does.
///
/// Storage is `ceil(n/64)` `u64` words per row, so a 24-movement junction costs 384
/// bytes for both matrices.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ConflictMatrix {
    n: u32,
    foes: Vec<u64>,
    response: Vec<u64>,
}

impl ConflictMatrix {
    /// An all-false matrix for `n` movements.
    pub fn new(n: usize) -> Self {
        let words = Self::words_per_row(n) * n;
        Self {
            n: n as u32,
            foes: vec![0; words],
            response: vec![0; words],
        }
    }

    const fn words_per_row(n: usize) -> usize {
        n.div_ceil(64)
    }

    /// How many movements the matrix covers.
    pub fn len(&self) -> usize {
        self.n as usize
    }

    /// True if the matrix covers no movements.
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    fn bit(&self, words: &[u64], a: usize, b: usize) -> bool {
        if a >= self.len() || b >= self.len() {
            return false;
        }
        let w = Self::words_per_row(self.len());
        words[a * w + b / 64] & (1u64 << (b % 64)) != 0
    }

    fn set_bit(words: &mut [u64], n: usize, a: usize, b: usize, value: bool) {
        let w = Self::words_per_row(n);
        let mask = 1u64 << (b % 64);
        let word = &mut words[a * w + b / 64];
        if value {
            *word |= mask;
        } else {
            *word &= !mask;
        }
    }

    /// Records that movements `a` and `b` conflict (symmetrically).
    ///
    /// Out-of-range indices are ignored, so a partially built junction cannot panic a
    /// generator.
    pub fn set_foe(&mut self, a: usize, b: usize, value: bool) {
        if a >= self.len() || b >= self.len() {
            return;
        }
        let n = self.len();
        Self::set_bit(&mut self.foes, n, a, b, value);
        Self::set_bit(&mut self.foes, n, b, a, value);
    }

    /// Records that movement `a` must give way to movement `b`.
    pub fn set_response(&mut self, a: usize, b: usize, value: bool) {
        if a >= self.len() || b >= self.len() {
            return;
        }
        let n = self.len();
        Self::set_bit(&mut self.response, n, a, b, value);
    }

    /// True if movements `a` and `b` conflict.
    pub fn is_foe(&self, a: usize, b: usize) -> bool {
        self.bit(&self.foes, a, b)
    }

    /// True if movement `a` must give way to movement `b`.
    pub fn must_yield(&self, a: usize, b: usize) -> bool {
        self.bit(&self.response, a, b)
    }

    /// The movements that conflict with `a`, in index order.
    pub fn foes_of(&self, a: usize) -> Vec<usize> {
        (0..self.len()).filter(|b| self.is_foe(a, *b)).collect()
    }

    /// The raw bit words, for the serialisers and the content hash.
    pub fn raw(&self) -> (&[u64], &[u64]) {
        (&self.foes, &self.response)
    }

    /// Rebuilds a matrix from raw words, checking their length.
    pub fn from_raw(n: usize, foes: Vec<u64>, response: Vec<u64>) -> Option<Self> {
        let want = Self::words_per_row(n) * n;
        if foes.len() != want || response.len() != want {
            return None;
        }
        Some(Self {
            n: n as u32,
            foes,
            response,
        })
    }
}

/// A junction of the road network.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Junction {
    /// This junction's dense id.
    pub id: JunctionId,
    /// Its reference point: the centre of the junction area.
    pub position: Vec3,
    /// The junction area as a polygon ring, counter-clockwise and **closed** (the last
    /// point repeats the first). May be empty when the source gave no area.
    pub shape: Vec<Vec3>,
    /// The lanes that arrive here, in id order.
    pub incoming: Vec<LaneId>,
    /// The lanes that leave here, in id order.
    pub outgoing: Vec<LaneId>,
    /// The internal connectors, one per movement, in movement order. This is also the
    /// row order of [`Junction::conflicts`].
    pub internal: Vec<LaneId>,
    /// How the junction is controlled.
    pub control: JunctionControl,
    /// Which movements conflict and who yields.
    pub conflicts: ConflictMatrix,
    /// The junction's name, if the source gave one.
    pub name: Option<SymbolId>,
}

impl Junction {
    /// How far outside its own shape a junction's position may measure and still count as
    /// on the boundary: 1 µm.
    ///
    /// The invariant itself is exact — `RoadNetwork::quantise_in_place` hulls the stored
    /// position together with the stored shape, so mathematically the distance is zero —
    /// but [`position_is_in_shape`](Junction::position_is_in_shape) has to *measure* it,
    /// and at city-scale coordinates the squared-distance arithmetic carries a rounding
    /// error of the order of a nanometre. A micrometre is a thousand times that, and still
    /// 240 times tighter than the 2.43e-4 m by which the worst Manhattan junction used to
    /// sit outside its polygon, so the check catches the defect it was written for.
    pub const SHAPE_TOLERANCE_M: f64 = 1e-6;

    /// True if [`Junction::position`] lies inside [`Junction::shape`], or on its boundary
    /// to within [`Junction::SHAPE_TOLERANCE_M`].
    ///
    /// Trivially true for a junction with no shape: a cul-de-sac has no area, and the
    /// model and the wire payload both allow an empty one.
    ///
    /// The boundary counts as inside. [`point_in_ring`] uses the crossing-number
    /// convention, which reports a point exactly on a vertex or on a "right" edge as
    /// outside, and a junction's position *is* one of the hull's own points, so on the
    /// boundary is the normal case rather than the exception — 715 of Manhattan's 3 421
    /// junctions sit exactly there.
    pub fn position_is_in_shape(&self) -> bool {
        if self.shape.is_empty() {
            return true;
        }
        point_in_ring(&self.shape, self.position)
            || ring_distance_sq_2d(&self.shape, self.position)
                <= Self::SHAPE_TOLERANCE_M * Self::SHAPE_TOLERANCE_M
    }
}

/// One movement across a junction, or one lane-to-lane continuation.
///
/// A movement through a junction appears **twice**: once as `approach → departure` with
/// its internal connector in `via`, and once as `connector → departure` with `via` empty.
/// A router that plans on the abstract graph uses the first, a mobility model that drives
/// the geometry follows the second, and both see the same set of movements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    /// The lane the movement leaves.
    pub from_lane: LaneId,
    /// The lane the movement enters.
    pub to_lane: LaneId,
    /// The internal connector traversed on the way, if any.
    pub via: Option<LaneId>,
    /// Which way it turns.
    pub direction: TurnDirection,
    /// True if the movement is legally permitted (a turn restriction sets it false while
    /// keeping the geometry, so the UI can show a banned turn).
    pub permitted: bool,
}

impl Connection {
    /// The sort key that [`RoadNetwork`] orders connections by.
    ///
    /// `via` sorts as `u32::MAX` when absent, so the `approach → departure` record (which
    /// carries a connector) precedes nothing in particular but is ordered deterministically
    /// against its siblings.
    pub fn sort_key(&self) -> (u32, u32, u32) {
        (
            self.from_lane.index(),
            self.to_lane.index(),
            self.via.map_or(u32::MAX, |v| v.index()),
        )
    }
}

/// A pedestrian or cyclist crossing of a road.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Crossing {
    /// This crossing's dense id.
    pub id: CrossingId,
    /// The junction it belongs to.
    pub junction: JunctionId,
    /// One end of the crossing line.
    pub from: Vec3,
    /// The other end.
    pub to: Vec3,
    /// Crossing width, metres (the painted band, across the direction of walking).
    pub width_m: f64,
    /// True if crossing traffic has priority (a zebra rather than an unmarked crossing).
    pub priority: bool,
}

/// The lane-level road graph: lanes, edges, junctions, connections and crossings.
///
/// # Id assignment
///
/// Every collection is dense and indexed by its id: `lanes[i].id == LaneId(i)`. An
/// importer or generator must therefore assign ids in a deterministic order and hand them
/// over already sorted; [`RoadNetwork::new`] checks it. [`crate::procedural`] documents
/// the order it uses.
///
/// # Connection ordering
///
/// [`RoadNetwork::successors`] hands out a subslice of one shared `Vec<Connection>`, so
/// the connections are sorted by `(from_lane, to_lane, via)` and indexed by a compressed
/// row offset table built on first use. The table is derived data, so it is not
/// serialised; it is rebuilt lazily and is a pure function of the connection list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoadNetwork {
    lanes: Vec<Lane>,
    edges: Vec<Edge>,
    junctions: Vec<Junction>,
    connections: Vec<Connection>,
    crossings: Vec<Crossing>,
    #[serde(skip)]
    successor_offsets: OnceLock<Vec<u32>>,
}

impl PartialEq for RoadNetwork {
    /// Compares the data, not the lazily built offset table, which is derived from it.
    fn eq(&self, other: &Self) -> bool {
        self.lanes == other.lanes
            && self.edges == other.edges
            && self.junctions == other.junctions
            && self.connections == other.connections
            && self.crossings == other.crossings
    }
}

impl RoadNetwork {
    /// Builds a road network, sorting the connections into the order
    /// [`RoadNetwork::successors`] needs and checking that every id is dense.
    ///
    /// # Errors
    ///
    /// [`WorldError::NonDenseIds`] if a collection's ids are not `0, 1, 2, …`.
    pub fn new(
        lanes: Vec<Lane>,
        edges: Vec<Edge>,
        junctions: Vec<Junction>,
        mut connections: Vec<Connection>,
        crossings: Vec<Crossing>,
    ) -> Result<Self> {
        for (i, l) in lanes.iter().enumerate() {
            if l.id.as_usize() != i {
                return Err(WorldError::NonDenseIds {
                    kind: "lane",
                    index: i as u32,
                    found: l.id.index(),
                });
            }
        }
        for (i, e) in edges.iter().enumerate() {
            if e.id.as_usize() != i {
                return Err(WorldError::NonDenseIds {
                    kind: "edge",
                    index: i as u32,
                    found: e.id.index(),
                });
            }
        }
        for (i, j) in junctions.iter().enumerate() {
            if j.id.as_usize() != i {
                return Err(WorldError::NonDenseIds {
                    kind: "junction",
                    index: i as u32,
                    found: j.id.index(),
                });
            }
        }
        for (i, c) in crossings.iter().enumerate() {
            if c.id.as_usize() != i {
                return Err(WorldError::NonDenseIds {
                    kind: "crossing",
                    index: i as u32,
                    found: c.id.index(),
                });
            }
        }
        connections.sort_by_key(Connection::sort_key);
        Ok(Self {
            lanes,
            edges,
            junctions,
            connections,
            crossings,
            successor_offsets: OnceLock::new(),
        })
    }

    /// Every lane, in id order.
    pub fn lanes(&self) -> &[Lane] {
        &self.lanes
    }

    /// Every edge, in id order.
    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }

    /// Every junction, in id order.
    pub fn junctions(&self) -> &[Junction] {
        &self.junctions
    }

    /// Every connection, sorted by `(from_lane, to_lane, via)`.
    pub fn connections(&self) -> &[Connection] {
        &self.connections
    }

    /// Every crossing, in id order (03-interfaces.md §2).
    pub fn crossings(&self) -> &[Crossing] {
        &self.crossings
    }

    /// The lane with this id (03-interfaces.md §2).
    ///
    /// # Panics
    ///
    /// If the id is out of range. Ids are dense and never reused within a run, so a live
    /// id is always valid; use [`RoadNetwork::try_lane`] when the id came from outside
    /// the engine (a scenario file, an RPC).
    pub fn lane(&self, id: LaneId) -> &Lane {
        &self.lanes[id.as_usize()]
    }

    /// The lane with this id, or `None`.
    pub fn try_lane(&self, id: LaneId) -> Option<&Lane> {
        self.lanes.get(id.as_usize())
    }

    /// The edge with this id.
    ///
    /// # Panics
    ///
    /// If the id is out of range; see [`RoadNetwork::lane`].
    pub fn edge(&self, id: EdgeId) -> &Edge {
        &self.edges[id.as_usize()]
    }

    /// The edge with this id, or `None`.
    pub fn try_edge(&self, id: EdgeId) -> Option<&Edge> {
        self.edges.get(id.as_usize())
    }

    /// The junction with this id (03-interfaces.md §2).
    ///
    /// # Panics
    ///
    /// If the id is out of range; see [`RoadNetwork::lane`].
    pub fn junction(&self, id: JunctionId) -> &Junction {
        &self.junctions[id.as_usize()]
    }

    /// The junction with this id, or `None`.
    pub fn try_junction(&self, id: JunctionId) -> Option<&Junction> {
        self.junctions.get(id.as_usize())
    }

    /// The connections leaving `lane` (03-interfaces.md §2).
    ///
    /// The slice is sorted by `(to_lane, via)`, so a router iterating it visits successors
    /// in a fixed order whatever the thread count.
    pub fn successors(&self, lane: LaneId) -> &[Connection] {
        let offsets = self.successor_offsets();
        let i = lane.as_usize();
        if i + 1 >= offsets.len() {
            return &[];
        }
        &self.connections[offsets[i] as usize..offsets[i + 1] as usize]
    }

    fn successor_offsets(&self) -> &[u32] {
        self.successor_offsets.get_or_init(|| {
            let mut offsets = vec![0u32; self.lanes.len() + 1];
            for c in &self.connections {
                if c.from_lane.as_usize() < self.lanes.len() {
                    offsets[c.from_lane.as_usize() + 1] += 1;
                }
            }
            for i in 0..self.lanes.len() {
                offsets[i + 1] += offsets[i];
            }
            offsets
        })
    }

    /// Drops the lazily built successor table. Called by [`World::reindex`].
    pub(crate) fn reindex(&mut self) {
        self.successor_offsets = OnceLock::new();
    }

    /// How many lanes, edges, junctions, connections and crossings the network holds.
    pub fn counts(&self) -> NetworkCounts {
        NetworkCounts {
            lanes: self.lanes.len(),
            edges: self.edges.len(),
            junctions: self.junctions.len(),
            connections: self.connections.len(),
            crossings: self.crossings.len(),
        }
    }

    /// The total centreline length of every lane, metres — a cheap sanity number for
    /// tests and for the import report.
    pub fn total_lane_length_m(&self) -> f64 {
        self.lanes.iter().map(|l| l.length_m).sum()
    }
}

/// The sizes of a [`RoadNetwork`], for tests and import reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkCounts {
    /// Number of lanes, internal connectors included.
    pub lanes: usize,
    /// Number of edges, synthetic internal edges included.
    pub edges: usize,
    /// Number of junctions.
    pub junctions: usize,
    /// Number of connection records.
    pub connections: usize,
    /// Number of crossings.
    pub crossings: usize,
}

// ---------------------------------------------------------------------------
// Buildings
// ---------------------------------------------------------------------------

/// The construction material of a building, for the obstacle models of
/// 04-models.md §3.5.
///
/// The discriminants are the `material` codes of docs/protocol/vwp-v1.md §4.4.
/// [`MaterialClass::Unknown`] is the default and is only replaced from an explicit tag
/// (04-models.md §1.3) — the simulator never guesses a material from a country or a
/// building age.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
#[repr(u8)]
pub enum MaterialClass {
    /// Not stated by the source.
    #[default]
    Unknown = 0,
    /// Concrete.
    Concrete = 1,
    /// Brick or masonry.
    Brick = 2,
    /// Glass curtain wall.
    Glass = 3,
    /// Timber.
    Wood = 4,
    /// Sheet metal.
    Metal = 5,
}

impl MaterialClass {
    /// The `material` code of docs/protocol/vwp-v1.md §4.4.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }

    /// The spelling the `vwp-world/1` JSON form uses (§4.6).
    pub const fn wire_name(self) -> &'static str {
        match self {
            MaterialClass::Unknown => "unknown",
            MaterialClass::Concrete => "concrete",
            MaterialClass::Brick => "brick",
            MaterialClass::Glass => "glass",
            MaterialClass::Wood => "wood",
            MaterialClass::Metal => "metal",
        }
    }

    /// The class for a wire code, or `None`.
    pub const fn from_wire_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(MaterialClass::Unknown),
            1 => Some(MaterialClass::Concrete),
            2 => Some(MaterialClass::Brick),
            3 => Some(MaterialClass::Glass),
            4 => Some(MaterialClass::Wood),
            5 => Some(MaterialClass::Metal),
            _ => None,
        }
    }
}

/// How a building's height was obtained.
///
/// 04-models.md §1.3 makes the height-defaulting rule provenance-tracked: OSM `height`
/// tag coverage is sparse and unquantified, so a consumer must be able to tell a surveyed
/// height from `levels × metres_per_level` from a land-use default. The propagation
/// model's uncertainty depends on it, and so does whether a "why" answer may claim a
/// source.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum HeightSource {
    /// From an explicit `height` tag: ground contact to roof top.
    Tagged,
    /// Derived as `building:levels × metres_per_level`.
    FromLevels,
    /// From the `building:part` volumes inside the outline: the tallest part's top,
    /// because the outline's own tags described only its base (V1).
    ///
    /// OSM *Simple 3D Buildings* lets a mapper leave the `building=*` outline untagged
    /// for height and put every height on the parts inside it. Midtown is mapped that
    /// way: the Chrysler Building's outline carries no `height` at all and its 279 m
    /// tower is a part. A height with this source is as surveyed as
    /// [`HeightSource::Tagged`] — it came from a `height` tag — but the tag was on a
    /// part, and the outline it is attached to is only the part's footprint neighbour,
    /// so a consumer that cares about which volume is where must say so.
    FromParts,
    /// The land-use default, because the source gave neither.
    #[default]
    Defaulted,
}

impl HeightSource {
    /// A short label for the provenance document and the "why" panel.
    pub const fn label(self) -> &'static str {
        match self {
            HeightSource::Tagged => "tagged",
            HeightSource::FromLevels => "from-levels",
            HeightSource::FromParts => "from-parts",
            HeightSource::Defaulted => "defaulted",
        }
    }
}

/// How much detail a renderer should give a building.
///
/// The discriminants are the `lod_hint` codes of docs/protocol/vwp-v1.md §4.4.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
#[repr(u8)]
pub enum LodHint {
    /// An extruded footprint.
    #[default]
    Box = 0,
    /// An extruded footprint with a roof shape.
    BoxRoof = 1,
    /// A detailed model exists for this building.
    Detailed = 2,
}

impl LodHint {
    /// The `lod_hint` code of docs/protocol/vwp-v1.md §4.4.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }

    /// The spelling the `vwp-world/1` JSON form uses (§4.6).
    pub const fn wire_name(self) -> &'static str {
        match self {
            LodHint::Box => "box",
            LodHint::BoxRoof => "box+roof",
            LodHint::Detailed => "detailed",
        }
    }

    /// The hint for a wire code, or `None`.
    pub const fn from_wire_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(LodHint::Box),
            1 => Some(LodHint::BoxRoof),
            2 => Some(LodHint::Detailed),
            _ => None,
        }
    }
}

/// A building footprint and its height: the obstacle the propagation models see and the
/// prism the renderer extrudes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Building {
    /// This building's dense id.
    pub id: BuildingId,
    /// The outer ring, **counter-clockwise** and **closed** (the last point repeats the
    /// first), world-local metres. Each point's `z` is the ground height there.
    ///
    /// The wire payload drops the repeated last point, because
    /// docs/protocol/vwp-v1.md §4.4 says rings are not closed there; the model keeps it
    /// so that ring arithmetic (area, winding, point-in-polygon) needs no wrap-around
    /// special case.
    ///
    /// **Exception, documented rather than silent:** a ring whose signed area is exactly
    /// zero — all points collinear, or a self-intersecting bow-tie whose lobes cancel —
    /// has no winding, so §4.4's counter-clockwise rule says nothing about it. Such a
    /// ring is stored in the canonical order described on `normalise_ring` instead of
    /// being wound, and [`ring_signed_area_2x`] on it returns zero, which is how a
    /// consumer can recognise one.
    pub footprint: Vec<Vec3>,
    /// Interior holes, each a closed ring wound **clockwise** (opposite the outer ring).
    ///
    /// `vwp-world/1` stores outer rings only and records the drop in the world
    /// provenance (§4.4); the model keeps holes because a courtyard is a real gap in an
    /// obstacle and a higher-tier obstacle model will want it.
    pub holes: Vec<Vec<Vec3>>,
    /// Height above [`Building::base_z_m`], metres.
    pub height_m: f64,
    /// Height of the bottom of the built part above the base, metres — OSM `min_height`
    /// / `building:min_level`. Zero for a building that meets the ground.
    pub min_height_m: f64,
    /// Ground height under the footprint, metres: the minimum `z` of the outer ring.
    pub base_z_m: f64,
    /// Storeys, when the source gave them. The wire payload writes `0xFFFF` for `None`.
    pub levels: Option<u16>,
    /// Construction material.
    pub material: MaterialClass,
    /// Where the height came from (04-models.md §1.3, invariant I-W3).
    pub height_source: HeightSource,
    /// Render detail hint.
    pub lod: LodHint,
    /// The building's name, if the source gave one.
    pub name: Option<SymbolId>,
}

impl Building {
    /// Builds a building, quantising its geometry and closing its rings.
    ///
    /// The outer ring is wound counter-clockwise and closed, and every hole is wound
    /// clockwise and closed, whatever order the caller gave; an importer reading OSM gets
    /// both windings in the wild and should not have to care.
    ///
    /// # Errors
    ///
    /// [`WorldError::ShortRing`] if the outer ring has fewer than three distinct points,
    /// or [`WorldError::NonFinite`] for a bad coordinate.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: BuildingId,
        footprint: impl IntoIterator<Item = Vec3>,
        holes: impl IntoIterator<Item = Vec<Vec3>>,
        height_m: f64,
        min_height_m: f64,
        material: MaterialClass,
        height_source: HeightSource,
    ) -> Result<Self> {
        let outer = close_ring(normalise_ring(
            footprint.into_iter().map(quantise_vec3).collect::<Vec<_>>(),
            true,
        ));
        if outer.len() < 4 {
            return Err(WorldError::ShortRing {
                what: format!("building {id} footprint"),
                points: outer.len().saturating_sub(1),
            });
        }
        for p in &outer {
            if !p.is_finite() {
                return Err(WorldError::NonFinite {
                    what: format!("building {id} footprint"),
                });
            }
        }
        let mut cut = Vec::new();
        for h in holes {
            let ring = close_ring(normalise_ring(
                h.into_iter().map(quantise_vec3).collect::<Vec<_>>(),
                false,
            ));
            if ring.len() >= 4 {
                cut.push(ring);
            }
        }
        let lowest = outer.iter().map(|p| p.z).fold(f64::INFINITY, f64::min);
        let base_z_m = if lowest.is_finite() { lowest } else { 0.0 };
        Ok(Self {
            id,
            footprint: outer,
            holes: cut,
            height_m: quantise(height_m, Q_HEIGHT_M),
            min_height_m: quantise(min_height_m, Q_HEIGHT_M),
            base_z_m: quantise(base_z_m, Q_HEIGHT_M),
            levels: None,
            material,
            height_source,
            lod: LodHint::Box,
            name: None,
        })
    }

    /// The roof height in world `z`: `base_z_m + height_m`.
    pub fn roof_z_m(&self) -> f64 {
        self.base_z_m + self.height_m
    }

    /// The outer ring without its repeated closing point — the form the wire payload
    /// wants (docs/protocol/vwp-v1.md §4.4).
    pub fn open_ring(&self) -> &[Vec3] {
        let n = self.footprint.len();
        if n >= 2 && self.footprint[0] == self.footprint[n - 1] {
            &self.footprint[..n - 1]
        } else {
            &self.footprint
        }
    }

    /// The footprint's axis-aligned bounding box, including the building's height.
    pub fn bbox(&self) -> Bbox {
        let mut b = Bbox::from_points(self.footprint.iter().copied());
        b.include(Vec3::new(b.min.x, b.min.y, self.roof_z_m()));
        b
    }

    /// True if `p` is inside the outer ring and outside every hole, in the horizontal
    /// plane.
    pub fn contains_2d(&self, p: Vec3) -> bool {
        if !point_in_ring(&self.footprint, p) {
            return false;
        }
        !self.holes.iter().any(|h| point_in_ring(h, p))
    }
}

/// Twice the signed area of a closed ring, in the horizontal plane.
///
/// Positive means counter-clockwise. Multiplication and addition only, so it is exact on
/// every platform; the shoelace sum is taken in point order, so it is also independent of
/// where the ring starts to within floating-point associativity.
pub fn ring_signed_area_2x(ring: &[Vec3]) -> f64 {
    let mut acc = 0.0;
    for w in ring.windows(2) {
        acc += w[0].x * w[1].y - w[1].x * w[0].y;
    }
    if let (Some(first), Some(last)) = (ring.first(), ring.last()) {
        if first.x != last.x || first.y != last.y {
            acc += last.x * first.y - first.x * last.y;
        }
    }
    acc
}

/// True if `p` is inside `ring` in the horizontal plane (ray casting, boundary included
/// only on the "left" edges, which is the usual crossing-number convention).
pub fn point_in_ring(ring: &[Vec3], p: Vec3) -> bool {
    let n = ring.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (a, b) = (ring[i], ring[j]);
        if (a.y > p.y) != (b.y > p.y) {
            let t = (p.y - a.y) / (b.y - a.y);
            if p.x < a.x + t * (b.x - a.x) {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// The **squared** horizontal distance from `p` to a ring's boundary, or `f64::INFINITY`
/// for a ring with fewer than two points.
///
/// Squared, because that is enough for every comparison the crate makes with it and it
/// costs no `sqrt`: multiplication, addition and one division only, so the answer is
/// identical on every platform without going through `v2xw_core::math` at all. The ring
/// is treated as the polyline through its points, so a **closed** ring (the form the model
/// stores) has all of its edges considered and an open one is missing its closing edge.
pub fn ring_distance_sq_2d(ring: &[Vec3], p: Vec3) -> f64 {
    let mut best = f64::INFINITY;
    for w in ring.windows(2) {
        let (a, b) = (w[0], w[1]);
        let ab = b - a;
        let len2 = ab.x * ab.x + ab.y * ab.y;
        let t = if len2 <= 0.0 {
            0.0
        } else {
            (((p.x - a.x) * ab.x + (p.y - a.y) * ab.y) / len2).clamp(0.0, 1.0)
        };
        let c = a.lerp(b, t);
        let d = (p.x - c.x) * (p.x - c.x) + (p.y - c.y) * (p.y - c.y);
        if d < best {
            best = d;
        }
    }
    best
}

/// The counter-clockwise convex hull of `points` as a **closed** ring (the last point
/// repeats the first), or an empty vector when there are fewer than three distinct
/// points or they are all collinear.
///
/// Andrew's monotone chain: a sort and two linear passes, with `f64::total_cmp` as the
/// order so that the result does not depend on how the platform compares
/// equal-but-signed zeroes, and cross products only — no division, no transcendental —
/// so the hull of a given point set is the same on every platform. Collinear points are
/// dropped, so the ring's vertices are exactly its corners.
///
/// This is the junction area of 04-models.md §1.2 (the polygon spanned by the ends of the
/// lanes that meet there), and it is here rather than in an importer because
/// `RoadNetwork::quantise_in_place` has to be able to re-establish the hull on the
/// quantised values. `osm::build_junction_shapes` still carries its own copy of the
/// algorithm; that copy should be deleted in favour of this one.
pub fn convex_hull_ring(points: &[Vec3]) -> Vec<Vec3> {
    let mut sorted: Vec<Vec3> = points.to_vec();
    sorted.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    sorted.dedup_by(|a, b| a.x == b.x && a.y == b.y);
    if sorted.len() < 3 {
        return Vec::new();
    }
    let cross = |o: Vec3, a: Vec3, b: Vec3| (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x);
    let mut hull: Vec<Vec3> = Vec::with_capacity(sorted.len() * 2);
    for &p in &sorted {
        while hull.len() >= 2 && cross(hull[hull.len() - 2], hull[hull.len() - 1], p) <= 0.0 {
            hull.pop();
        }
        hull.push(p);
    }
    let lower = hull.len() + 1;
    for &p in sorted.iter().rev() {
        while hull.len() >= lower && cross(hull[hull.len() - 2], hull[hull.len() - 1], p) <= 0.0 {
            hull.pop();
        }
        hull.push(p);
    }
    if hull.len() < 4 {
        return Vec::new();
    }
    hull
}

/// A closed ring without its repeated closing point, as a fresh vector.
fn open_ring_points(ring: &[Vec3]) -> Vec<Vec3> {
    let n = ring.len();
    if n >= 2 && ring[0] == ring[n - 1] {
        ring[..n - 1].to_vec()
    } else {
        ring.to_vec()
    }
}

/// Simplifies a polyline with the Ramer-Douglas-Peucker algorithm, keeping both ends.
///
/// `tolerance_m` is the greatest distance, in the horizontal plane, that a dropped point
/// may lie from the polyline that replaces it. A non-positive or non-finite tolerance, or
/// a polyline of fewer than three points, returns a copy.
///
/// This is the simplification 04-models.md §1.2 records for the OSM importer (the legacy
/// fetcher used RDP at a 10 m tolerance, escalating it until its node cap held); it lives
/// here because it is geometry, and because an importer that rolls its own would be a
/// second implementation to keep deterministic. It is: the perpendicular distance is a
/// cross product over a `sqrt`, both IEEE-754 exact, and the recursion is replaced by an
/// explicit stack whose order is fixed, so the same input gives the same output
/// everywhere.
///
/// The distance is horizontal. A road's `z` is carried along unchanged, because dropping
/// a point for being a few centimetres off in height would be a gradient decision, not a
/// shape one.
///
/// ```
/// use v2xw_core::geom::Vec3;
/// use v2xw_world::model::simplify_rdp;
///
/// let line = [
///     Vec3::new_2d(0.0, 0.0),
///     Vec3::new_2d(5.0, 0.01),   // nearly collinear
///     Vec3::new_2d(10.0, 0.0),
///     Vec3::new_2d(10.0, 10.0),
/// ];
/// let kept = simplify_rdp(&line, 0.5);
/// assert_eq!(kept.len(), 3);
/// assert_eq!(kept[0], line[0]);
/// assert_eq!(kept[2], line[3]);
/// ```
pub fn simplify_rdp(points: &[Vec3], tolerance_m: f64) -> Vec<Vec3> {
    if points.len() < 3 || !(tolerance_m.is_finite() && tolerance_m > 0.0) {
        return points.to_vec();
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut stack = vec![(0usize, points.len() - 1)];
    while let Some((first, last)) = stack.pop() {
        if last <= first + 1 {
            continue;
        }
        let (a, b) = (points[first], points[last]);
        let ab = b - a;
        let len = ab.norm_2d();
        let mut worst = 0.0;
        let mut worst_at = first;
        for (i, p) in points.iter().enumerate().take(last).skip(first + 1) {
            let d = if len == 0.0 {
                (*p - a).norm_2d()
            } else {
                // Twice the triangle's area over its base: the perpendicular distance.
                ((p.x - a.x) * ab.y - (p.y - a.y) * ab.x).abs() / len
            };
            if d > worst {
                worst = d;
                worst_at = i;
            }
        }
        if worst > tolerance_m {
            keep[worst_at] = true;
            stack.push((first, worst_at));
            stack.push((worst_at, last));
        }
    }
    points
        .iter()
        .zip(keep)
        .filter_map(|(p, k)| if k { Some(*p) } else { None })
        .collect()
}

/// Drops a ring's repeated closing point, then rewinds it counter-clockwise (or
/// clockwise when `counter_clockwise` is false).
///
/// # The degenerate case, and what is done with it
///
/// A ring whose signed area is exactly zero has **no winding to correct**: the two
/// orientations are indistinguishable by area. Two rings reach this state — one whose
/// points are all collinear, and a self-intersecting ("figure-of-eight" or bow-tie) one
/// whose lobes cancel. The winding test used to fall through for both, leaving the ring
/// in whatever order the source happened to give it, so the same footprint imported from
/// two sources that list its nodes in opposite orders produced two different payloads —
/// while docs/protocol/vwp-v1.md §4.4 declares payload rings counter-clockwise.
///
/// Such a ring is **not rejected**: dropping it would silently delete a real (if badly
/// drawn) obstacle, and no anomaly category covers it. Instead the fallback is made
/// explicit and deterministic — [`canonical_ring`] replaces the source order with the
/// one canonical order of that cycle, so the same degenerate footprint listed from any
/// starting node, in either direction, produces byte-identical output and therefore the
/// same content hash. [`ring_signed_area_2x`] stays zero either way, so a consumer that
/// needs to know can still tell that this ring bounds no area, and
/// [`point_in_ring`] answers `false` for every point of a bow-tie, as it did before.
fn normalise_ring(mut ring: Vec<Vec3>, counter_clockwise: bool) -> Vec<Vec3> {
    if ring.len() >= 2 && ring[0] == ring[ring.len() - 1] {
        ring.pop();
    }
    let area = ring_signed_area_2x(&ring);
    if area == 0.0 {
        return canonical_ring(&ring);
    }
    if (area < 0.0) == counter_clockwise {
        ring.reverse();
    }
    ring
}

/// A total order on points: `x`, then `y`, then `z`, by `f64::total_cmp`.
///
/// `total_cmp` rather than `<`, so the order does not depend on how a platform compares
/// signed zeroes and cannot be made non-transitive by a `NaN`.
fn cmp_point(a: &Vec3, b: &Vec3) -> core::cmp::Ordering {
    a.x.total_cmp(&b.x)
        .then(a.y.total_cmp(&b.y))
        .then(a.z.total_cmp(&b.z))
}

/// Compares the rotation of `points` starting at `i` with the one starting at `j`.
fn cmp_rotation(points: &[Vec3], i: usize, j: usize) -> core::cmp::Ordering {
    let n = points.len();
    for k in 0..n {
        let order = cmp_point(&points[(i + k) % n], &points[(j + k) % n]);
        if order != core::cmp::Ordering::Equal {
            return order;
        }
    }
    core::cmp::Ordering::Equal
}

/// The start index of the lexicographically smallest rotation of `points`.
fn best_rotation(points: &[Vec3]) -> usize {
    let mut best = 0usize;
    for start in 1..points.len() {
        if cmp_rotation(points, start, best) == core::cmp::Ordering::Less {
            best = start;
        }
    }
    best
}

/// The canonical spelling of an open ring: the lexicographically smallest of its `2n`
/// readings (`n` rotations, each in two directions).
///
/// Used only by [`normalise_ring`], and only for a ring whose signed area is zero, where
/// there is no winding to normalise and therefore nothing else that makes two spellings
/// of the same cycle converge. `O(n²)` in the ring length, which is why it is not used
/// for ordinary rings: they are already canonical to within their starting point, which
/// the source chose and which carries no ambiguity once the winding is fixed.
fn canonical_ring(ring: &[Vec3]) -> Vec<Vec3> {
    let n = ring.len();
    if n < 2 {
        return ring.to_vec();
    }
    let reversed: Vec<Vec3> = ring.iter().rev().copied().collect();
    let rotate = |src: &[Vec3], start: usize| -> Vec<Vec3> {
        (0..n).map(|k| src[(start + k) % n]).collect()
    };
    let forward = rotate(ring, best_rotation(ring));
    let backward = rotate(&reversed, best_rotation(&reversed));
    let order = forward
        .iter()
        .zip(backward.iter())
        .map(|(a, b)| cmp_point(a, b))
        .find(|o| *o != core::cmp::Ordering::Equal)
        .unwrap_or(core::cmp::Ordering::Equal);
    if order == core::cmp::Ordering::Greater {
        backward
    } else {
        forward
    }
}

/// Appends the first point to close a ring, if it is not closed already.
fn close_ring(mut ring: Vec<Vec3>) -> Vec<Vec3> {
    if let Some(&first) = ring.first() {
        if ring.last() != Some(&first) {
            ring.push(first);
        }
    }
    ring
}

// ---------------------------------------------------------------------------
// Signals
// ---------------------------------------------------------------------------

/// The state of one movement during one signal phase.
///
/// The names are the SUMO `tlLogic` state letters, which the SUMO importer maps directly
/// (04-models.md §1.2). They are *not* the SAE J2735 `MovementPhaseState` numbers the
/// wire protocol uses for live signal state (docs/protocol/vwp-v1.md §3.3.3): this enum
/// describes the static plan, and the engine maps it to J2735 when it publishes state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SignalState {
    /// Red: stop.
    Red,
    /// Red and amber together: prepare to go (not used in every country).
    RedAmber,
    /// Amber: stop if you safely can.
    Amber,
    /// Green with right of way.
    Green,
    /// Green, but the movement must give way to a conflicting movement — a permissive
    /// green left turn. The junction's [`ConflictMatrix`] says to whom.
    GreenYield,
    /// Flashing amber: proceed with care, no right of way.
    FlashingAmber,
    /// The signal head is dark for this movement.
    Off,
}

impl SignalState {
    /// The SUMO `tlLogic` state letter.
    pub const fn sumo_letter(self) -> char {
        match self {
            SignalState::Red => 'r',
            SignalState::RedAmber => 'u',
            SignalState::Amber => 'y',
            SignalState::Green => 'G',
            SignalState::GreenYield => 'g',
            SignalState::FlashingAmber => 'o',
            SignalState::Off => 'O',
        }
    }

    /// True if a vehicle may enter the junction on this state.
    pub const fn permits_entry(self) -> bool {
        matches!(
            self,
            SignalState::Green | SignalState::GreenYield | SignalState::FlashingAmber
        )
    }
}

/// One phase of a fixed-time plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignalPhase {
    /// How long the phase lasts, seconds.
    pub duration_s: f64,
    /// The state of each controlled movement, parallel to
    /// [`SignalPlan::controlled`].
    pub states: Vec<SignalState>,
    /// A label for the UI, e.g. `"east-west green"`.
    pub name: Option<SymbolId>,
}

/// What a signal head faces.
///
/// The discriminants are the `kind` codes of docs/protocol/vwp-v1.md §4.5.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
#[repr(u8)]
pub enum SignalHeadKind {
    /// A vehicle head.
    #[default]
    Vehicle = 0,
    /// A pedestrian head.
    Pedestrian = 1,
    /// A cycle head.
    Bicycle = 2,
    /// A transit (tram or bus) head.
    Transit = 3,
}

impl SignalHeadKind {
    /// The `kind` code of docs/protocol/vwp-v1.md §4.5.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }

    /// The spelling the `vwp-world/1` JSON form uses (§4.6).
    pub const fn wire_name(self) -> &'static str {
        match self {
            SignalHeadKind::Vehicle => "vehicle",
            SignalHeadKind::Pedestrian => "pedestrian",
            SignalHeadKind::Bicycle => "bicycle",
            SignalHeadKind::Transit => "transit",
        }
    }
}

/// One physical signal head: where the lantern is and which lane it faces.
///
/// The wire payload's signal section is a list of heads (docs/protocol/vwp-v1.md §4.5),
/// because that is what a renderer draws; the plan is what the engine runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignalHead {
    /// The lane the head faces — the approach lane a driver reads it from.
    pub lane: LaneId,
    /// Where the lantern is, world-local metres.
    pub position: Vec3,
    /// What it faces.
    pub kind: SignalHeadKind,
    /// The phase group it belongs to: heads in one group always show the same state.
    pub group: u16,
}

/// A fixed-time signal plan for one junction.
///
/// 04-models.md §1.1 lists signal plans as first-class world data, and 04-models.md §1.2
/// records that an OSM import gets only signal *presence*: the plan itself is generated by
/// `mobility/intersection/signal-fixed-time` defaults. This struct is what such a
/// generator produces and what the SUMO importer fills from `<tlLogic>`.
///
/// # Invariants
///
/// Checked by [`World::validate`]:
///
/// * every phase's `states` has one entry per [`SignalPlan::controlled`] movement;
/// * the phase durations sum to [`SignalPlan::cycle_s`] to within one quantum;
/// * `offset_s` is in `[0, cycle_s)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignalPlan {
    /// This plan's dense id, which is also the controller's id.
    pub id: SignalId,
    /// The junction it controls.
    pub junction: JunctionId,
    /// Cycle length, seconds.
    pub cycle_s: f64,
    /// Offset of the cycle start from `t0`, seconds — how this junction is coordinated
    /// with its neighbours (a green wave).
    pub offset_s: f64,
    /// The movements this plan controls, as internal-lane ids, in the order the phase
    /// state vectors use. Usually [`Junction::internal`] in the same order.
    pub controlled: Vec<LaneId>,
    /// The phases, in cycle order.
    pub phases: Vec<SignalPhase>,
    /// The physical heads, for rendering.
    pub heads: Vec<SignalHead>,
}

impl SignalPlan {
    /// The index of the phase running at `t_s` seconds after `t0`, and how far into it.
    ///
    /// Arithmetic only: `%` on `f64` is exact in IEEE-754, so two engines agree on the
    /// phase boundary to the last bit, which matters because a phase change is a
    /// threshold comparison (D10).
    pub fn phase_at(&self, t_s: f64) -> Option<(usize, f64)> {
        if self.phases.is_empty() || !(self.cycle_s.is_finite() && self.cycle_s > 0.0) {
            return None;
        }
        let mut into = (t_s - self.offset_s) % self.cycle_s;
        if into < 0.0 {
            into += self.cycle_s;
        }
        let mut acc = 0.0;
        for (i, p) in self.phases.iter().enumerate() {
            if into < acc + p.duration_s {
                return Some((i, into - acc));
            }
            acc += p.duration_s;
        }
        // Rounding of the durations can leave `into` a hair past the last boundary.
        Some((
            self.phases.len() - 1,
            into - acc + self.phases[self.phases.len() - 1].duration_s,
        ))
    }

    /// The state of every controlled movement at `t_s`.
    pub fn states_at(&self, t_s: f64) -> Option<&[SignalState]> {
        let (i, _) = self.phase_at(t_s)?;
        Some(&self.phases[i].states)
    }

    /// The sum of the phase durations, seconds.
    pub fn total_phase_duration_s(&self) -> f64 {
        self.phases.iter().map(|p| p.duration_s).sum()
    }

    /// What each signal group of this plan's heads shows through the cycle, as
    /// `(group, [(state, duration_s)])`, groups ascending, phases merged where the
    /// group's state does not change (so a duration is the time to the next *change*).
    ///
    /// A group shows the most permissive state among the movements whose approach lane
    /// carries one of its heads (Green over GreenYield over FlashingAmber over Amber over
    /// RedAmber over Red over Off): a head over an approach shows the through movement's
    /// green while the left turn from the same approach has a permissive one.
    /// `approach_of` maps a controlled (internal) lane to the lane that approaches it.
    ///
    /// This is what a renderer colours a head with. The plan's own phase states are per
    /// *movement*; picking any one movement's state for the whole controller — the first,
    /// as the live stream once did — shows every head the state of one approach.
    pub fn group_timelines(
        &self,
        approach_of: impl Fn(LaneId) -> Option<LaneId>,
    ) -> Vec<(u16, Vec<(SignalState, f64)>)> {
        let mut groups: Vec<u16> = self.heads.iter().map(|h| h.group).collect();
        groups.sort_unstable();
        groups.dedup();
        let group_of: Vec<Option<u16>> = self
            .controlled
            .iter()
            .map(|l| {
                let approach = approach_of(*l)?;
                self.heads
                    .iter()
                    .find(|h| h.lane == approach)
                    .map(|h| h.group)
            })
            .collect();
        let rank = |s: SignalState| match s {
            SignalState::Green => 6,
            SignalState::GreenYield => 5,
            SignalState::FlashingAmber => 4,
            SignalState::Amber => 3,
            SignalState::RedAmber => 2,
            SignalState::Red => 1,
            SignalState::Off => 0,
        };
        groups
            .into_iter()
            .map(|g| {
                let mut timeline: Vec<(SignalState, f64)> = Vec::new();
                for phase in &self.phases {
                    let state = phase
                        .states
                        .iter()
                        .zip(&group_of)
                        .filter(|(_, og)| **og == Some(g))
                        .map(|(s, _)| *s)
                        .max_by_key(|s| rank(*s))
                        .unwrap_or(SignalState::Off);
                    match timeline.last_mut() {
                        Some((last, d)) if *last == state => *d += phase.duration_s,
                        _ => timeline.push((state, phase.duration_s)),
                    }
                }
                (g, timeline)
            })
            .collect()
    }
}

/// The id a signal *group* goes by on the live stream's signal block
/// (docs/protocol/vwp-v1.md §3.3.3): `(controller + 1) · 65536 + group`.
///
/// Always at least 65536, so a plain controller id (below 65536) stays available for a
/// producer that knows only a controller's state, and a consumer can tell the two apart.
pub const fn signal_group_wire_id(plan: SignalId, group: u16) -> u32 {
    (plan.index() + 1)
        .wrapping_mul(65536)
        .wrapping_add(group as u32)
}

// ---------------------------------------------------------------------------
// Sites, land use, terrain
// ---------------------------------------------------------------------------

/// What kind of infrastructure site this is.
///
/// The discriminants are the `kind` codes of docs/protocol/vwp-v1.md §4.5.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
#[repr(u8)]
pub enum SiteKind {
    /// A roadside unit mast.
    #[default]
    Rsu = 0,
    /// A cellular site.
    Cell = 1,
    /// Anything else worth a position.
    Other = 2,
}

impl SiteKind {
    /// The `kind` code of docs/protocol/vwp-v1.md §4.5.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }

    /// The spelling the `vwp-world/1` JSON form uses (§4.6).
    pub const fn wire_name(self) -> &'static str {
        match self {
            SiteKind::Rsu => "rsu",
            SiteKind::Cell => "cell",
            SiteKind::Other => "other",
        }
    }
}

/// A candidate or assigned infrastructure position: where an RSU or a cell could stand.
///
/// A site is world geometry — it exists whether or not the scenario puts a node on it.
/// [`Site::node`] is filled once the scenario assigns one, which is why it is optional
/// and why the wire payload has a `0xFFFFFFFF` sentinel for it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Site {
    /// This site's dense id.
    pub id: SiteId,
    /// The node standing here, once the scenario has assigned one.
    pub node: Option<NodeId>,
    /// Ground position, world-local metres.
    pub position: Vec3,
    /// Antenna height above the ground position, metres.
    pub antenna_height_m: f64,
    /// Antenna gain, dBi.
    pub antenna_gain_dbi: f64,
    /// What kind of site it is.
    pub kind: SiteKind,
    /// The site's name, if it has one.
    pub name: Option<SymbolId>,
}

impl Site {
    /// The antenna's phase centre: the ground position raised by the mast height.
    ///
    /// This is the point [`v2xw_core`]'s `RadioEndpoint` wants (03-interfaces.md §4).
    pub fn antenna_position(&self) -> Vec3 {
        Vec3::new(
            self.position.x,
            self.position.y,
            self.position.z + self.antenna_height_m,
        )
    }
}

/// The propagation environment class of a place (03-interfaces.md §4,
/// `Propagation::environment`).
///
/// Four classes, because that is what the path-loss parameter presets of 04-models.md §3
/// are indexed by.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum EnvClass {
    /// Dense built-up area: tall buildings on both sides, heavy clutter.
    #[default]
    Urban,
    /// Low-rise built-up area with gardens and gaps.
    Suburban,
    /// A motorway corridor: few obstacles, high speeds.
    Highway,
    /// Open country.
    Rural,
}

impl EnvClass {
    /// A short label for parameter-preset lookup and the "why" panel.
    pub const fn label(self) -> &'static str {
        match self {
            EnvClass::Urban => "urban",
            EnvClass::Suburban => "suburban",
            EnvClass::Highway => "highway",
            EnvClass::Rural => "rural",
        }
    }
}

/// The land-use category of a zone, as the renderer and the wire payload see it.
///
/// The discriminants are the `class` codes of **docs/protocol/vwp-v1.md §4.5**:
/// `0` urban, `1` suburban, `2` rural, `3` highway, `4` water, `5` park, `6` industrial.
///
/// Note for implementers: the TypeScript client's `LANDUSE_CLASSES` table used to list
/// `park` at 4 and `water` at 5, the other way round from the specification, so every
/// zone this crate wrote decoded as the wrong class in the viewer — Central Park as open
/// water. The specification is normative and this crate always followed it, so the client
/// was what was wrong; it was corrected on 2026-09-18 and `world.test.ts` now pins each
/// name to the §4.5 code it is documented with. Nothing else in the payload was affected,
/// because the class byte is used for nothing but the fill colour and
/// [`LanduseClass::default_env`].
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
#[repr(u8)]
pub enum LanduseClass {
    /// Dense built-up area.
    #[default]
    Urban = 0,
    /// Low-rise built-up area.
    Suburban = 1,
    /// Open country.
    Rural = 2,
    /// A motorway corridor.
    Highway = 3,
    /// Open water.
    Water = 4,
    /// A park or other green space.
    Park = 5,
    /// An industrial estate.
    Industrial = 6,
}

impl LanduseClass {
    /// The `class` code of docs/protocol/vwp-v1.md §4.5.
    pub const fn wire_code(self) -> u8 {
        self as u8
    }

    /// The spelling the `vwp-world/1` JSON form uses (§4.6).
    pub const fn wire_name(self) -> &'static str {
        match self {
            LanduseClass::Urban => "urban",
            LanduseClass::Suburban => "suburban",
            LanduseClass::Rural => "rural",
            LanduseClass::Highway => "highway",
            LanduseClass::Water => "water",
            LanduseClass::Park => "park",
            LanduseClass::Industrial => "industrial",
        }
    }

    /// The class for a wire code, or `None`.
    pub const fn from_wire_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(LanduseClass::Urban),
            1 => Some(LanduseClass::Suburban),
            2 => Some(LanduseClass::Rural),
            3 => Some(LanduseClass::Highway),
            4 => Some(LanduseClass::Water),
            5 => Some(LanduseClass::Park),
            6 => Some(LanduseClass::Industrial),
            _ => None,
        }
    }

    /// The propagation environment an importer should assume for this land use, unless
    /// it knows better.
    ///
    /// **This mapping is a default, not a calibrated model.** Three of the seven
    /// land-use categories have no propagation class of their own, so they are mapped to
    /// the class whose clutter they most resemble: a park and open water have no
    /// buildings, an industrial estate has large low blocks. 04-models.md §1.2 already
    /// marks the environment class defaulted from an import as `TODO: calibrate`
    /// (the plan there is to compare defaulted classes with hand-labelled ones on the
    /// Phase 2 scenarios); this table is part of what that work will check. An importer
    /// that has better information sets [`LanduseZone::env`] directly and records the
    /// rule it used in the provenance.
    pub const fn default_env(self) -> EnvClass {
        match self {
            LanduseClass::Urban => EnvClass::Urban,
            LanduseClass::Suburban | LanduseClass::Industrial => EnvClass::Suburban,
            LanduseClass::Rural | LanduseClass::Water | LanduseClass::Park => EnvClass::Rural,
            LanduseClass::Highway => EnvClass::Highway,
        }
    }
}

/// A land-use zone: a polygon with a category and a propagation environment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LanduseZone {
    /// This zone's dense id.
    pub id: ZoneId,
    /// The zone boundary: counter-clockwise, closed, world-local metres.
    pub ring: Vec<Vec3>,
    /// The land-use category, for rendering and for the wire payload.
    pub class: LanduseClass,
    /// The propagation environment inside the zone (03-interfaces.md §4).
    ///
    /// Stored rather than derived, because an importer may know better than
    /// [`LanduseClass::default_env`] and the choice must be recorded, not recomputed.
    pub env: EnvClass,
    /// The zone's name, if the source gave one.
    pub name: Option<SymbolId>,
}

impl LanduseZone {
    /// Builds a zone, quantising and closing its ring counter-clockwise.
    ///
    /// # Errors
    ///
    /// [`WorldError::ShortRing`] if the ring has fewer than three distinct points.
    pub fn new(
        id: ZoneId,
        ring: impl IntoIterator<Item = Vec3>,
        class: LanduseClass,
        env: EnvClass,
    ) -> Result<Self> {
        let ring = close_ring(normalise_ring(
            ring.into_iter().map(quantise_vec3).collect::<Vec<_>>(),
            true,
        ));
        if ring.len() < 4 {
            return Err(WorldError::ShortRing {
                what: format!("landuse zone {id} ring"),
                points: ring.len().saturating_sub(1),
            });
        }
        Ok(Self {
            id,
            ring,
            class,
            env,
            name: None,
        })
    }

    /// The ring without its repeated closing point — the form the wire payload wants.
    pub fn open_ring(&self) -> &[Vec3] {
        let n = self.ring.len();
        if n >= 2 && self.ring[0] == self.ring[n - 1] {
            &self.ring[..n - 1]
        } else {
            &self.ring
        }
    }

    /// True if `p` is inside the zone, in the horizontal plane.
    pub fn contains_2d(&self, p: Vec3) -> bool {
        point_in_ring(&self.ring, p)
    }
}

/// How a terrain grid is interpolated between its samples.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum Interpolation {
    /// Bilinear between the four surrounding samples — the rule 04-models.md §1.4
    /// records for the DEM resampling.
    #[default]
    Bilinear,
    /// The nearest sample, for a categorical grid.
    Nearest,
}

impl Interpolation {
    /// A stable lower-case label, used by the content hash and the provenance record.
    pub const fn label(self) -> &'static str {
        match self {
            Interpolation::Bilinear => "bilinear",
            Interpolation::Nearest => "nearest",
        }
    }
}

/// A digital elevation model resampled onto a regular world-local grid
/// (04-models.md §1.4).
///
/// Sample `(ix, iy)` sits at `(origin_x_m + ix·cell_x_m, origin_y_m + iy·cell_y_m)` and
/// lives at `heights_m[iy · nx + ix]`: row-major, south to north, west to east. Heights
/// are quantised to [`crate::quant::Q_HEIGHT_M`] like every other height, and are `f64`
/// rather than `f32` because a DEM can carry Himalayan altitudes, where `f32` no longer
/// holds a millimetre.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Terrain {
    /// World-local `x` of sample column 0, metres.
    pub origin_x_m: f64,
    /// World-local `y` of sample row 0, metres.
    pub origin_y_m: f64,
    /// Column spacing, metres.
    pub cell_x_m: f64,
    /// Row spacing, metres.
    pub cell_y_m: f64,
    /// Number of columns.
    pub nx: u32,
    /// Number of rows.
    pub ny: u32,
    /// Heights, row-major, `nx · ny` entries, metres above the world's `z = 0`.
    pub heights_m: Vec<f64>,
    /// How to interpolate between samples.
    pub interpolation: Interpolation,
    /// The DEM this was resampled from, e.g. `world/terrain/copernicus-glo-30`.
    pub source: Option<SymbolId>,
}

impl Terrain {
    /// Builds a terrain grid, quantising its heights.
    ///
    /// # Errors
    ///
    /// [`WorldError::InvalidParameter`] if the grid is empty, the spacing is not
    /// positive, or the height count does not match `nx · ny`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        origin_x_m: f64,
        origin_y_m: f64,
        cell_x_m: f64,
        cell_y_m: f64,
        nx: u32,
        ny: u32,
        heights_m: Vec<f64>,
        interpolation: Interpolation,
    ) -> Result<Self> {
        if nx < 2 || ny < 2 {
            return Err(WorldError::InvalidParameter {
                parameter: "terrain.nx/ny".to_string(),
                problem: format!("a grid needs at least 2 × 2 samples, got {nx} × {ny}"),
            });
        }
        if !(cell_x_m.is_finite() && cell_x_m > 0.0 && cell_y_m.is_finite() && cell_y_m > 0.0) {
            return Err(WorldError::InvalidParameter {
                parameter: "terrain.cell_x_m/cell_y_m".to_string(),
                problem: format!("spacing must be positive, got {cell_x_m} × {cell_y_m}"),
            });
        }
        if heights_m.len() != nx as usize * ny as usize {
            return Err(WorldError::InvalidParameter {
                parameter: "terrain.heights_m".to_string(),
                problem: format!("{} heights for a {nx} × {ny} grid", heights_m.len()),
            });
        }
        Ok(Self {
            origin_x_m: quantise(origin_x_m, Q_POSITION_M),
            origin_y_m: quantise(origin_y_m, Q_POSITION_M),
            cell_x_m: quantise(cell_x_m, Q_POSITION_M),
            cell_y_m: quantise(cell_y_m, Q_POSITION_M),
            nx,
            ny,
            heights_m: heights_m
                .into_iter()
                .map(|h| quantise(h, Q_HEIGHT_M))
                .collect(),
            interpolation,
            source: None,
        })
    }

    /// The sample at `(ix, iy)`, or `None` outside the grid.
    pub fn sample_at(&self, ix: u32, iy: u32) -> Option<f64> {
        if ix >= self.nx || iy >= self.ny {
            return None;
        }
        self.heights_m
            .get(iy as usize * self.nx as usize + ix as usize)
            .copied()
    }

    /// The grid's extent in world-local metres.
    pub fn extent(&self) -> Bbox {
        Bbox::new(
            Vec3::new(self.origin_x_m, self.origin_y_m, 0.0),
            Vec3::new(
                self.origin_x_m + self.cell_x_m * f64::from(self.nx - 1),
                self.origin_y_m + self.cell_y_m * f64::from(self.ny - 1),
                0.0,
            ),
        )
    }

    /// The interpolated height at `(x, y)`, or `None` outside the grid.
    ///
    /// Bilinear interpolation, multiplication and addition only, so it is exact on every
    /// platform. The result is **not** quantised: a sampled height is an intermediate
    /// value, and the writer quantises whatever it puts in an artefact.
    pub fn height_at(&self, x_m: f64, y_m: f64) -> Option<f64> {
        let fx = (x_m - self.origin_x_m) / self.cell_x_m;
        let fy = (y_m - self.origin_y_m) / self.cell_y_m;
        if fx < 0.0 || fy < 0.0 || fx > f64::from(self.nx - 1) || fy > f64::from(self.ny - 1) {
            return None;
        }
        let ix = (fx.floor() as u32).min(self.nx - 2);
        let iy = (fy.floor() as u32).min(self.ny - 2);
        let tx = fx - f64::from(ix);
        let ty = fy - f64::from(iy);
        let h00 = self.sample_at(ix, iy)?;
        let h10 = self.sample_at(ix + 1, iy)?;
        let h01 = self.sample_at(ix, iy + 1)?;
        let h11 = self.sample_at(ix + 1, iy + 1)?;
        match self.interpolation {
            Interpolation::Nearest => {
                let ix = if tx >= 0.5 { ix + 1 } else { ix };
                let iy = if ty >= 0.5 { iy + 1 } else { iy };
                self.sample_at(ix, iy)
            }
            Interpolation::Bilinear => {
                let bottom = h00 + (h10 - h00) * tx;
                let top = h01 + (h11 - h01) * tx;
                Some(bottom + (top - bottom) * ty)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Provenance
// ---------------------------------------------------------------------------

/// Where a world came from (04-models.md §1.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorldSourceKind {
    /// An OpenStreetMap extract.
    Osm,
    /// A SUMO `net.xml` network.
    SumoNet,
    /// An ASAM OpenDRIVE file, imported through `netconvert`.
    OpenDrive,
    /// A procedural generator.
    Procedural,
    /// The legacy engine's node/edge JSON.
    JsonLegacy,
    /// A world built by hand, in code or in a test.
    Synthetic,
}

impl WorldSourceKind {
    /// The label used in the provenance document and the wire payload.
    pub const fn label(self) -> &'static str {
        match self {
            WorldSourceKind::Osm => "osm",
            WorldSourceKind::SumoNet => "sumo-net",
            WorldSourceKind::OpenDrive => "opendrive",
            WorldSourceKind::Procedural => "procedural",
            WorldSourceKind::JsonLegacy => "json-legacy",
            WorldSourceKind::Synthetic => "synthetic",
        }
    }
}

/// One transformation an importer applied, with the parameters it used (invariant I-W3).
///
/// 04-models.md §1.5 requires every transformation to be recorded with its parameters:
/// the projection, the simplification tolerance, the junction join distance, the signal
/// guessing options, the height defaulting rule and its fit id, the DEM resampling.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transformation {
    /// What was applied, e.g. `simplify` or `height-default`.
    pub name: String,
    /// Its parameters, ordered by key so the record is byte-stable.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, serde_json::Value>,
}

impl Transformation {
    /// A transformation with no parameters.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            params: BTreeMap::new(),
        }
    }

    /// Adds a parameter, returning `self` for chaining.
    ///
    /// A **floating-point** parameter is quantised on the way in, to the grid
    /// [`Transformation::param_quantum`] infers from its key. Everything else — an
    /// integer, a boolean, a string, a nested value — is stored verbatim.
    ///
    /// The reason is D9, which is not limited to geometry: a transformation's parameters
    /// are serialised into every artefact this crate writes (`world.json`, `world.v2xw`,
    /// the `.vwb` provenance blob, `serde_vwp::to_json_string`), so a raw `f64` here is a
    /// raw IEEE-754 double in an exported, digested artefact just as much as a coordinate
    /// is. Quantising at the recording call is the writer-side encoder D9 asks for;
    /// [`World::scan_exported_floats`] then visits these parameters as well, so a
    /// `params` map filled in by hand rather than through this method is still caught by
    /// [`World::validate`].
    #[must_use]
    pub fn with(mut self, key: impl Into<String>, value: impl Into<serde_json::Value>) -> Self {
        let key = key.into();
        let mut value = value.into();
        if let serde_json::Value::Number(n) = &value {
            if n.is_f64() {
                if let Some(on_grid) = n
                    .as_f64()
                    .map(|x| quantise(x, Self::param_quantum(&key)))
                    .and_then(serde_json::Number::from_f64)
                {
                    value = serde_json::Value::Number(on_grid);
                }
            }
        }
        self.params.insert(key, value);
        self
    }

    /// The quantum a float parameter called `key` is recorded on (D9's grid table, as
    /// [`crate::quant`] declares it).
    ///
    /// A `params` map is free-form JSON, so unlike every other float in the crate these
    /// values have no declared field to carry a quantum. The key's own spelling is the
    /// only declaration available, and every parameter this crate records is named for
    /// its unit:
    ///
    /// | Key | Quantum |
    /// |---|---|
    /// | contains `lat` or `lon`, ends with `_deg`, or is `degrees` | [`crate::quant::Q_DEGREES`] |
    /// | ends with `_mps` | [`crate::quant::Q_SPEED_MPS`] |
    /// | ends with `_s` | [`crate::quant::Q_TIME_S`] |
    /// | ends with `_m` | [`crate::quant::Q_POSITION_M`] |
    /// | ends with `_db` or `_dbi`, or is `db` | [`crate::quant::Q_DB`] |
    /// | anything else | [`crate::quant::Q_DEGREES`] |
    ///
    /// The fallback is the *finest* grid in the table on purpose: it puts the value on a
    /// grid, which is what D9 requires, without coarsening a quantity whose unit this
    /// function cannot name. An importer that wants a coarser grid for a parameter names
    /// the parameter for its unit, which it should be doing anyway.
    pub fn param_quantum(key: &str) -> f64 {
        if key.contains("lat") || key.contains("lon") || key.ends_with("_deg") || key == "degrees" {
            Q_DEGREES
        } else if key.ends_with("_mps") {
            Q_SPEED_MPS
        } else if key.ends_with("_s") {
            Q_TIME_S
        } else if key.ends_with("_m") {
            Q_POSITION_M
        } else if key.ends_with("_db") || key.ends_with("_dbi") || key == "db" {
            Q_DB
        } else {
            Q_DEGREES
        }
    }

    /// Every float parameter, with the grid it is declared on — what the D9 scan visits.
    fn float_params(&self) -> impl Iterator<Item = (&String, f64, f64)> {
        self.params.iter().filter_map(|(key, value)| match value {
            serde_json::Value::Number(n) if n.is_f64() => Some((
                key,
                n.as_f64().unwrap_or(f64::NAN),
                Self::param_quantum(key),
            )),
            _ => None,
        })
    }

    /// The one-line form the `vwp-world/1` provenance document uses, e.g.
    /// `simplify:tolerance_m=0.25`.
    ///
    /// docs/protocol/vwp-v1.md §4.6 shows `transformations` as an array of strings, so
    /// the structured record is flattened for the wire: the name, then, if it has
    /// parameters, a colon and `key=value` pairs in key order joined by commas.
    pub fn to_wire_string(&self) -> String {
        if self.params.is_empty() {
            return self.name.clone();
        }
        let joined = self
            .params
            .iter()
            .map(|(k, v)| format!("{k}={}", json_scalar_to_string(v)))
            .collect::<Vec<_>>()
            .join(",");
        format!("{}:{joined}", self.name)
    }
}

/// Renders a JSON scalar without its quotes, so `"0.25"` and `0.25` both print `0.25`.
fn json_scalar_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// The licence of one layer of a world, and the attribution it requires
/// (04-models.md §1.5, 08-measurement-and-data.md §9).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LayerLicence {
    /// Which layer: `roads`, `buildings`, `terrain`, `landuse`, …
    pub layer: String,
    /// The licence identifier, e.g. `ODbL-1.0`, `CC-BY-4.0`, `Apache-2.0`.
    pub licence: String,
    /// The attribution string that must be reproduced, verbatim, by anything that
    /// republishes this layer. Copernicus and Overture both require an exact wording
    /// (04-models.md §1.4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution: Option<String>,
}

impl LayerLicence {
    /// A layer licence with no attribution requirement.
    pub fn new(layer: impl Into<String>, licence: impl Into<String>) -> Self {
        Self {
            layer: layer.into(),
            licence: licence.into(),
            attribution: None,
        }
    }

    /// A layer licence with a required attribution string.
    pub fn with_attribution(
        layer: impl Into<String>,
        licence: impl Into<String>,
        attribution: impl Into<String>,
    ) -> Self {
        Self {
            layer: layer.into(),
            licence: licence.into(),
            attribution: Some(attribution.into()),
        }
    }
}

/// Everything about where a world came from (03-interfaces.md §2, 04-models.md §1.5).
///
/// Invariant I-W3: an importer records **every** transformation it applied, with its
/// parameters. Invariant I-W2: [`WorldProvenance::content_hash`] is the world's content
/// hash, and is written to the run manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorldProvenance {
    /// What kind of source this came from.
    pub source: WorldSourceKind,
    /// Which source: a file hash, an Overpass bbox, a generator id and seed.
    pub source_id: String,
    /// The geodetic bounding box that was asked for, when the source had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_bbox: Option<GeoBbox>,
    /// When the import ran, ISO-8601 UTC.
    ///
    /// **Supplied by the caller.** No part of the engine may read the wall clock
    /// (02-architecture.md §6.1); the importer takes this string from
    /// [`crate::ImportOptions::imported_at`] and writes it down. It is excluded from the
    /// content hash for the same reason the manifest excludes its build timestamp.
    pub imported_at: String,
    /// The versions of every tool involved: this crate, `netconvert`, the OSM extract's
    /// generator.
    pub tool_versions: BTreeMap<String, String>,
    /// The projection applied, by id: [`Projection::NAME`].
    pub projection: String,
    /// The geodetic origin of the local plane.
    pub origin: GeoOrigin,
    /// Every transformation, in the order it was applied.
    pub transformations: Vec<Transformation>,
    /// The licence and attribution of every layer.
    pub layers: Vec<LayerLicence>,
    /// What the import dropped, by kind and count, e.g. `{"building_holes": 41}`.
    pub dropped: BTreeMap<String, u64>,
    /// Free-text notes an importer wants a human to read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// The world's content hash (I-W2), filled in by [`WorldBuilder::build`].
    #[serde(with = "hex32")]
    pub content_hash: [u8; 32],
}

impl WorldProvenance {
    /// A provenance record for a source, with no transformations yet.
    pub fn new(
        source: WorldSourceKind,
        source_id: impl Into<String>,
        imported_at: impl Into<String>,
        origin: GeoOrigin,
    ) -> Self {
        Self {
            source,
            source_id: source_id.into(),
            source_bbox: None,
            imported_at: imported_at.into(),
            tool_versions: BTreeMap::from([(
                "v2xw-world".to_string(),
                env!("CARGO_PKG_VERSION").to_string(),
            )]),
            projection: Projection::NAME.to_string(),
            origin,
            transformations: Vec::new(),
            layers: Vec::new(),
            dropped: BTreeMap::new(),
            notes: Vec::new(),
            content_hash: [0u8; 32],
        }
    }

    /// Records a transformation.
    pub fn record(&mut self, t: Transformation) {
        self.transformations.push(t);
    }

    /// Records that `count` more objects of `kind` were dropped.
    pub fn record_dropped(&mut self, kind: impl Into<String>, count: u64) {
        *self.dropped.entry(kind.into()).or_insert(0) += count;
    }

    /// Every attribution string this world's layers require, deduplicated, in layer
    /// order. An exporter must reproduce all of them (08-measurement-and-data.md §9).
    pub fn required_attributions(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for l in &self.layers {
            if let Some(a) = l.attribution.as_deref() {
                if !out.contains(&a) {
                    out.push(a);
                }
            }
        }
        out
    }

    /// The single `licence` string the `vwp-world/1` provenance document carries (§4.6).
    ///
    /// One licence for every layer collapses to that licence; a mixture is written as
    /// `layer=licence` pairs in layer order, because dropping the distinction would be a
    /// licence claim we cannot support.
    pub fn wire_licence(&self) -> String {
        if self.layers.is_empty() {
            return String::new();
        }
        let first = &self.layers[0].licence;
        if self.layers.iter().all(|l| &l.licence == first) {
            return first.clone();
        }
        self.layers
            .iter()
            .map(|l| format!("{}={}", l.layer, l.licence))
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// The provenance document the `vwp-world/1` payload embeds
    /// (docs/protocol/vwp-v1.md §4.5, §4.6).
    ///
    /// The shape is the one the specification's example shows —
    /// `{source, bbox, imported_at, tool_versions, transformations, licence, dropped}` —
    /// plus `attribution`, an array of the strings a republisher must reproduce, and
    /// `content_hash`. Both extras are additive keys, which §8.4 allows and which a
    /// reader that does not know them ignores.
    pub fn to_wire_json(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        map.insert("source".to_string(), self.source.label().into());
        map.insert("source_id".to_string(), self.source_id.clone().into());
        if let Some(b) = self.source_bbox {
            map.insert(
                "bbox".to_string(),
                serde_json::Value::Array(
                    b.to_lon_lat_array()
                        .iter()
                        .map(|v| serde_json::Value::from(*v))
                        .collect(),
                ),
            );
        }
        map.insert("imported_at".to_string(), self.imported_at.clone().into());
        map.insert(
            "tool_versions".to_string(),
            serde_json::Value::Object(
                self.tool_versions
                    .iter()
                    .map(|(k, v)| (k.clone(), serde_json::Value::from(v.clone())))
                    .collect(),
            ),
        );
        map.insert(
            "transformations".to_string(),
            serde_json::Value::Array(
                self.transformations
                    .iter()
                    .map(|t| serde_json::Value::from(t.to_wire_string()))
                    .collect(),
            ),
        );
        map.insert("licence".to_string(), self.wire_licence().into());
        map.insert(
            "attribution".to_string(),
            serde_json::Value::Array(
                self.required_attributions()
                    .into_iter()
                    .map(serde_json::Value::from)
                    .collect(),
            ),
        );
        map.insert(
            "dropped".to_string(),
            serde_json::Value::Object(
                self.dropped
                    .iter()
                    .map(|(k, v)| (k.clone(), serde_json::Value::from(*v)))
                    .collect(),
            ),
        );
        map.insert(
            "content_hash".to_string(),
            v2xw_core::hash::hex_encode(&self.content_hash).into(),
        );
        serde_json::Value::Object(map)
    }
}

// ---------------------------------------------------------------------------
// The world itself
// ---------------------------------------------------------------------------

/// One world: geometry, provenance and content hash (03-interfaces.md §2).
///
/// Build one with [`World::builder`], a [`crate::WorldSource`] such as
/// [`crate::procedural::GridSource`], or by reading one back with
/// [`crate::serde_native`].
///
/// # Immutability and the index cache
///
/// The data fields are public because 03-interfaces.md §2 says so and because every
/// consumer reads them. The spatial indices are **not** a field a consumer can touch:
/// they live behind a `OnceLock` that is filled on the first query and is rebuilt only by
/// [`World::reindex`], which needs `&mut self`. A shared `&World` therefore cannot
/// observe an index that disagrees with the data (invariant I-W1), and building the
/// index is a pure function of the data, so two engines that load the same world get the
/// same index.
#[derive(Debug, Serialize, Deserialize)]
pub struct World {
    /// The geodetic anchor of the local plane (D6: the bounding box's south-west
    /// corner).
    pub origin: GeoOrigin,
    /// The world's extent, world-local metres.
    pub bbox: Bbox,
    /// The lane-level road graph.
    pub roads: RoadNetwork,
    /// Buildings, in id order.
    pub buildings: Vec<Building>,
    /// The terrain grid, when the world has one.
    pub terrain: Option<Terrain>,
    /// Signal plans, in id order.
    pub signals: Vec<SignalPlan>,
    /// Infrastructure sites, in id order.
    pub sites: Vec<Site>,
    /// Land-use zones, in id order.
    pub landuse: Vec<LanduseZone>,
    /// The propagation environment outside every land-use zone.
    pub default_env: EnvClass,
    /// The world's interned strings.
    pub symbols: SymbolTable,
    /// Where it came from.
    pub provenance: WorldProvenance,
    /// SHA-256 over all geometry, in quantised form ([`crate::hash`], invariant I-W2).
    #[serde(with = "hex32")]
    pub content_hash: [u8; 32],
    /// How the lazily built spatial indices are sized.
    #[serde(default)]
    pub index_options: IndexOptions,
    #[serde(skip)]
    index: OnceLock<WorldIndex>,
}

impl Clone for World {
    /// Clones the data. The index cache is **not** copied: the clone builds its own on
    /// first use, which costs one traversal and keeps the two worlds independent.
    fn clone(&self) -> Self {
        Self {
            origin: self.origin,
            bbox: self.bbox,
            roads: self.roads.clone(),
            buildings: self.buildings.clone(),
            terrain: self.terrain.clone(),
            signals: self.signals.clone(),
            sites: self.sites.clone(),
            landuse: self.landuse.clone(),
            default_env: self.default_env,
            symbols: self.symbols.clone(),
            provenance: self.provenance.clone(),
            content_hash: self.content_hash,
            index_options: self.index_options,
            index: OnceLock::new(),
        }
    }
}

impl PartialEq for World {
    /// Compares the data, not the index cache, which is derived from it.
    fn eq(&self, other: &Self) -> bool {
        self.origin == other.origin
            && self.bbox == other.bbox
            && self.roads == other.roads
            && self.buildings == other.buildings
            && self.terrain == other.terrain
            && self.signals == other.signals
            && self.sites == other.sites
            && self.landuse == other.landuse
            && self.default_env == other.default_env
            && self.symbols == other.symbols
            && self.provenance == other.provenance
            && self.content_hash == other.content_hash
            && self.index_options == other.index_options
    }
}

impl World {
    /// Starts building a world anchored at `origin`.
    pub fn builder(origin: GeoOrigin) -> WorldBuilder {
        WorldBuilder::new(origin)
    }

    /// The projection between this world's metres and geodetic degrees.
    pub fn projection(&self) -> Projection {
        Projection::new(self.origin)
    }

    /// The spatial indices, built on first use.
    pub(crate) fn index(&self) -> &WorldIndex {
        self.index
            .get_or_init(|| WorldIndex::build(self, self.index_options))
    }

    /// Drops the cached spatial indices, so the next query rebuilds them.
    ///
    /// Only needed by code that mutates a world after building it — which invariant I-W1
    /// forbids for anything but the loader itself.
    pub fn reindex(&mut self) {
        self.index = OnceLock::new();
        self.roads.reindex();
    }

    /// The lane with this id (03-interfaces.md §2).
    ///
    /// # Panics
    ///
    /// If the id is out of range; see [`RoadNetwork::lane`].
    pub fn lane(&self, id: LaneId) -> &Lane {
        self.roads.lane(id)
    }

    /// The lane with this id, or `None`.
    pub fn try_lane(&self, id: LaneId) -> Option<&Lane> {
        self.roads.try_lane(id)
    }

    /// The junction with this id (03-interfaces.md §2).
    ///
    /// # Panics
    ///
    /// If the id is out of range.
    pub fn junction(&self, id: JunctionId) -> &Junction {
        self.roads.junction(id)
    }

    /// The edge with this id.
    ///
    /// # Panics
    ///
    /// If the id is out of range.
    pub fn edge(&self, id: EdgeId) -> &Edge {
        self.roads.edge(id)
    }

    /// The connections leaving `lane` (03-interfaces.md §2).
    pub fn successors(&self, lane: LaneId) -> &[Connection] {
        self.roads.successors(lane)
    }

    /// Every crossing (03-interfaces.md §2).
    pub fn crossings(&self) -> &[Crossing] {
        self.roads.crossings()
    }

    /// The building with this id, or `None`.
    pub fn building(&self, id: BuildingId) -> Option<&Building> {
        self.buildings.get(id.as_usize())
    }

    /// The signal plan with this id, or `None`.
    pub fn signal_plan(&self, id: SignalId) -> Option<&SignalPlan> {
        self.signals.get(id.as_usize())
    }

    /// The world-local point of a lane position (03-interfaces.md §2).
    ///
    /// # Panics
    ///
    /// If the lane id is out of range; use [`World::try_to_xyz`] for an id from outside
    /// the engine.
    pub fn to_xyz(&self, lp: &LanePos) -> Vec3 {
        self.lane(lp.lane).offset_point(lp.s_m, lp.d_m)
    }

    /// The world-local point of a lane position, or `None` if the lane does not exist.
    pub fn try_to_xyz(&self, lp: &LanePos) -> Option<Vec3> {
        Some(self.try_lane(lp.lane)?.offset_point(lp.s_m, lp.d_m))
    }

    /// The propagation environment at `p`: the class of the first land-use zone (in id
    /// order) that contains it, or [`World::default_env`].
    ///
    /// "First in id order" rather than "smallest zone" on purpose: it is a total,
    /// documented rule that costs nothing to reproduce, and overlapping land-use zones
    /// are an import defect the provenance should record rather than a case to arbitrate
    /// here.
    pub fn env_class_at(&self, p: Vec3) -> EnvClass {
        self.landuse
            .iter()
            .find(|z| z.contains_2d(p))
            .map_or(self.default_env, |z| z.env)
    }

    /// The ground height at `(x, y)`: the terrain sample if there is one, else `0.0`.
    pub fn ground_height_at(&self, x_m: f64, y_m: f64) -> f64 {
        self.terrain
            .as_ref()
            .and_then(|t| t.height_at(x_m, y_m))
            .unwrap_or(0.0)
    }

    /// The sizes of the road network, for tests and reports.
    pub fn counts(&self) -> NetworkCounts {
        self.roads.counts()
    }

    /// Checks every invariant this crate can check.
    ///
    /// Called by [`WorldBuilder::build`] and by every reader, so a world that exists is a
    /// world that passed. What it checks:
    ///
    /// * ids are dense in every collection;
    /// * every reference (lane → edge, connection → lane, junction → lane, signal →
    ///   junction, crossing → junction) resolves;
    /// * every lane's arc-length table matches its centreline to within a quantum, and is
    ///   strictly increasing;
    /// * connections are sorted the way [`RoadNetwork::successors`] needs;
    /// * every junction's conflict matrix matches its internal-lane count;
    /// * every signal plan's phases cover its cycle and have one state per movement;
    /// * every ring has at least three distinct points;
    /// * the bounding box contains every lane point, footprint and junction;
    /// * every exported float sits on its quantisation grid (D9).
    ///
    /// # Errors
    ///
    /// The first violation found, as a [`WorldError`].
    pub fn validate(&self) -> Result<()> {
        let lane_count = self.roads.lanes().len() as u32;
        let junction_count = self.roads.junctions().len() as u32;
        let edge_count = self.roads.edges().len() as u32;

        for lane in self.roads.lanes() {
            if lane.edge.index() >= edge_count {
                return Err(WorldError::DanglingReference {
                    what: format!("lane {}", lane.id),
                    kind: "edge",
                    id: lane.edge.index(),
                });
            }
            if let Some(j) = lane.junction {
                if j.index() >= junction_count {
                    return Err(WorldError::DanglingReference {
                        what: format!("lane {}", lane.id),
                        kind: "junction",
                        id: j.index(),
                    });
                }
            }
            if lane.centreline.len() < 2 {
                return Err(WorldError::ShortCentreline {
                    lane: lane.id,
                    points: lane.centreline.len(),
                });
            }
            if lane.cumulative.len() != lane.centreline.len() {
                return Err(WorldError::InconsistentArcLength {
                    lane: lane.id,
                    index: lane.cumulative.len(),
                    found: lane.cumulative.len() as f64,
                    expected: lane.centreline.len() as f64,
                    tolerance_m: 0.0,
                });
            }
            let expect = lane.recompute_cumulative();
            for (i, (found, want)) in lane.cumulative.iter().zip(expect.iter()).enumerate() {
                if (found - want).abs() > Q_POSITION_M {
                    return Err(WorldError::InconsistentArcLength {
                        lane: lane.id,
                        index: i,
                        found: *found,
                        expected: *want,
                        tolerance_m: Q_POSITION_M,
                    });
                }
                if i > 0 && *found <= lane.cumulative[i - 1] {
                    return Err(WorldError::InconsistentArcLength {
                        lane: lane.id,
                        index: i,
                        found: *found,
                        expected: lane.cumulative[i - 1],
                        tolerance_m: Q_POSITION_M,
                    });
                }
            }
        }

        for edge in self.roads.edges() {
            if edge.lanes.is_empty() {
                return Err(WorldError::EmptyEdge { edge: edge.id });
            }
            for l in &edge.lanes {
                if l.index() >= lane_count {
                    return Err(WorldError::DanglingReference {
                        what: format!("edge {}", edge.id),
                        kind: "lane",
                        id: l.index(),
                    });
                }
            }
            for j in [edge.from, edge.to] {
                if j.index() >= junction_count {
                    return Err(WorldError::DanglingReference {
                        what: format!("edge {}", edge.id),
                        kind: "junction",
                        id: j.index(),
                    });
                }
            }
        }

        for j in self.roads.junctions() {
            if j.conflicts.len() != j.internal.len() {
                return Err(WorldError::ConflictMatrixShape {
                    junction: j.id,
                    internal: j.internal.len(),
                    rows: j.conflicts.len(),
                });
            }
            for l in j.incoming.iter().chain(&j.outgoing).chain(&j.internal) {
                if l.index() >= lane_count {
                    return Err(WorldError::DanglingReference {
                        what: format!("junction {}", j.id),
                        kind: "lane",
                        id: l.index(),
                    });
                }
            }
            if let Some(plan) = j.control.plan() {
                if plan.as_usize() >= self.signals.len() {
                    return Err(WorldError::DanglingReference {
                        what: format!("junction {}", j.id),
                        kind: "signal plan",
                        id: plan.index(),
                    });
                }
            }
            // A junction's shape is the hull of its lane ends *and its own position*, so
            // the position belongs to the polygon. Independent quantisation of the two
            // used to be able to break that; `RoadNetwork::quantise_in_place` now
            // re-establishes it on the stored values, and this is the assertion that says
            // so — including for a world read back from an artefact, which never goes
            // through the quantiser.
            if !j.position_is_in_shape() {
                return Err(WorldError::Invariant {
                    invariant: "junction-shape",
                    problem: format!(
                        "junction {}'s position {:?} is {} m outside its own shape",
                        j.id,
                        j.position,
                        math::sqrt(ring_distance_sq_2d(&j.shape, j.position))
                    ),
                });
            }
        }

        let mut previous: Option<(u32, u32, u32)> = None;
        for (i, c) in self.roads.connections().iter().enumerate() {
            let key = c.sort_key();
            if let Some(p) = previous {
                if key < p {
                    return Err(WorldError::UnsortedConnections { index: i });
                }
            }
            previous = Some(key);
            for l in [Some(c.from_lane), Some(c.to_lane), c.via]
                .into_iter()
                .flatten()
            {
                if l.index() >= lane_count {
                    return Err(WorldError::DanglingReference {
                        what: format!("connection {i}"),
                        kind: "lane",
                        id: l.index(),
                    });
                }
            }
        }

        for c in self.roads.crossings() {
            if c.junction.index() >= junction_count {
                return Err(WorldError::DanglingReference {
                    what: format!("crossing {}", c.id),
                    kind: "junction",
                    id: c.junction.index(),
                });
            }
        }

        for (i, b) in self.buildings.iter().enumerate() {
            if b.id.as_usize() != i {
                return Err(WorldError::NonDenseIds {
                    kind: "building",
                    index: i as u32,
                    found: b.id.index(),
                });
            }
            if b.footprint.len() < 4 {
                return Err(WorldError::ShortRing {
                    what: format!("building {} footprint", b.id),
                    points: b.footprint.len().saturating_sub(1),
                });
            }
        }

        for (i, z) in self.landuse.iter().enumerate() {
            if z.id.as_usize() != i {
                return Err(WorldError::NonDenseIds {
                    kind: "landuse zone",
                    index: i as u32,
                    found: z.id.index(),
                });
            }
            if z.ring.len() < 4 {
                return Err(WorldError::ShortRing {
                    what: format!("landuse zone {} ring", z.id),
                    points: z.ring.len().saturating_sub(1),
                });
            }
        }

        for (i, s) in self.sites.iter().enumerate() {
            if s.id.as_usize() != i {
                return Err(WorldError::NonDenseIds {
                    kind: "site",
                    index: i as u32,
                    found: s.id.index(),
                });
            }
        }

        for (i, plan) in self.signals.iter().enumerate() {
            if plan.id.as_usize() != i {
                return Err(WorldError::NonDenseIds {
                    kind: "signal plan",
                    index: i as u32,
                    found: plan.id.index(),
                });
            }
            if plan.junction.index() >= junction_count {
                return Err(WorldError::DanglingReference {
                    what: format!("signal plan {}", plan.id),
                    kind: "junction",
                    id: plan.junction.index(),
                });
            }
            if plan.phases.is_empty() {
                return Err(WorldError::BadSignalPlan {
                    plan: plan.id.index(),
                    problem: "no phases".to_string(),
                });
            }
            if (plan.total_phase_duration_s() - plan.cycle_s).abs() > Q_TIME_S {
                return Err(WorldError::BadSignalPlan {
                    plan: plan.id.index(),
                    problem: format!(
                        "phases sum to {} s but the cycle is {} s",
                        plan.total_phase_duration_s(),
                        plan.cycle_s
                    ),
                });
            }
            if plan.offset_s < 0.0 || plan.offset_s >= plan.cycle_s {
                return Err(WorldError::BadSignalPlan {
                    plan: plan.id.index(),
                    problem: format!(
                        "offset {} s is outside [0, {})",
                        plan.offset_s, plan.cycle_s
                    ),
                });
            }
            for (k, phase) in plan.phases.iter().enumerate() {
                if phase.states.len() != plan.controlled.len() {
                    return Err(WorldError::BadSignalPlan {
                        plan: plan.id.index(),
                        problem: format!(
                            "phase {k} has {} states for {} controlled movements",
                            phase.states.len(),
                            plan.controlled.len()
                        ),
                    });
                }
                if !(phase.duration_s.is_finite() && phase.duration_s > 0.0) {
                    return Err(WorldError::BadSignalPlan {
                        plan: plan.id.index(),
                        problem: format!("phase {k} lasts {} s", phase.duration_s),
                    });
                }
            }
            for l in plan
                .controlled
                .iter()
                .chain(plan.heads.iter().map(|h| &h.lane))
            {
                if l.index() >= lane_count {
                    return Err(WorldError::DanglingReference {
                        what: format!("signal plan {}", plan.id),
                        kind: "lane",
                        id: l.index(),
                    });
                }
            }
        }

        // Every float that can reach an artefact: finite first, then on its grid.
        //
        // The finiteness pass is not redundant with `Lane::new` and `Building::new`,
        // which only check coordinates: a `NaN` height, base, width, speed limit, phase
        // duration or transformation parameter used to pass `build()` untouched, and the
        // two writers then disagreed about it — the binary payload stores the `NaN`
        // (docs/protocol/vwp-v1.md §0 makes it the "absent" sentinel) while the JSON form
        // turns it into `null`, so the two are not the "direct transcription" of each
        // other that §4.6 requires. Until §4.6 gains an encoding for the sentinel, a
        // world may not carry a non-finite float at all.
        let mut non_finite = None;
        let mut off_grid = None;
        self.scan_exported_floats(&mut |path, value, quantum| {
            if non_finite.is_none() && !value.is_finite() {
                non_finite = Some(format!("{path} (= {value})"));
            }
            if off_grid.is_none() && value.is_finite() && !crate::quant::is_on_grid(value, quantum)
            {
                off_grid = Some(format!(
                    "{path} = {value} is not a multiple of {quantum} (D9)"
                ));
            }
        });
        if let Some(what) = non_finite {
            return Err(WorldError::NonFinite { what });
        }
        if let Some(problem) = off_grid {
            return Err(WorldError::Invariant {
                invariant: "D9",
                problem,
            });
        }

        let mut extent = Bbox::empty();
        for lane in self.roads.lanes() {
            for p in &lane.centreline {
                extent.include(*p);
            }
        }
        for b in &self.buildings {
            for p in &b.footprint {
                extent.include(*p);
            }
        }
        if !extent.is_empty()
            && (extent.min.x < self.bbox.min.x - Q_POSITION_M
                || extent.min.y < self.bbox.min.y - Q_POSITION_M
                || extent.max.x > self.bbox.max.x + Q_POSITION_M
                || extent.max.y > self.bbox.max.y + Q_POSITION_M)
        {
            return Err(WorldError::Invariant {
                invariant: "bbox",
                problem: format!(
                    "geometry spans {:?}..{:?} but the bounding box is {:?}..{:?}",
                    extent.min, extent.max, self.bbox.min, self.bbox.max
                ),
            });
        }
        Ok(())
    }

    /// Visits every float that can reach a serialised artefact, with its quantum.
    ///
    /// This is the scanning pass D9 requires: `visit(field, value, quantum)` is called for
    /// each one, and the caller decides what to do. [`World::validate`] uses it to reject
    /// an off-grid world; the `d9_*` tests use it to prove the writers cannot emit one.
    ///
    /// `field` is a **static** path like `lane.centreline.x`, not an indexed one: the scan
    /// runs on every build and every read of a world with hundreds of thousands of points,
    /// and formatting an index per float would cost more than the check. An off-grid value
    /// is a property of a field, not of one object, so the field name and the value are
    /// what a report needs.
    ///
    /// Derived caches that are not written anywhere and the index structures are not
    /// visited, because they never reach an artefact. [`Lane::cumulative`] *is* written,
    /// so it is scanned.
    pub fn scan_exported_floats(&self, visit: &mut dyn FnMut(&str, f64, f64)) {
        visit("origin.lat_deg", self.origin.lat_deg, Q_DEGREES);
        visit("origin.lon_deg", self.origin.lon_deg, Q_DEGREES);
        visit("origin.alt_m", self.origin.alt_m, Q_HEIGHT_M);
        for v in [self.bbox.min, self.bbox.max] {
            visit("bbox.x", v.x, Q_POSITION_M);
            visit("bbox.y", v.y, Q_POSITION_M);
            visit("bbox.z", v.z, Q_HEIGHT_M);
        }
        for lane in self.roads.lanes() {
            for p in &lane.centreline {
                visit("lane.centreline.x", p.x, Q_POSITION_M);
                visit("lane.centreline.y", p.y, Q_POSITION_M);
                visit("lane.centreline.z", p.z, Q_HEIGHT_M);
            }
            for c in &lane.cumulative {
                visit("lane.cumulative", *c, Q_POSITION_M);
            }
            visit("lane.width_m", lane.width_m, Q_POSITION_M);
            visit("lane.length_m", lane.length_m, Q_POSITION_M);
            visit("lane.speed_limit_mps", lane.speed_limit_mps, Q_SPEED_MPS);
        }
        for j in self.roads.junctions() {
            visit("junction.position.x", j.position.x, Q_POSITION_M);
            visit("junction.position.y", j.position.y, Q_POSITION_M);
            visit("junction.position.z", j.position.z, Q_HEIGHT_M);
            for p in &j.shape {
                visit("junction.shape.x", p.x, Q_POSITION_M);
                visit("junction.shape.y", p.y, Q_POSITION_M);
                visit("junction.shape.z", p.z, Q_HEIGHT_M);
            }
        }
        for c in self.roads.crossings() {
            for p in [c.from, c.to] {
                visit("crossing.x", p.x, Q_POSITION_M);
                visit("crossing.y", p.y, Q_POSITION_M);
                visit("crossing.z", p.z, Q_HEIGHT_M);
            }
            visit("crossing.width_m", c.width_m, Q_POSITION_M);
        }
        for b in &self.buildings {
            for ring in core::iter::once(&b.footprint).chain(b.holes.iter()) {
                for p in ring {
                    visit("building.ring.x", p.x, Q_POSITION_M);
                    visit("building.ring.y", p.y, Q_POSITION_M);
                    visit("building.ring.z", p.z, Q_HEIGHT_M);
                }
            }
            visit("building.height_m", b.height_m, Q_HEIGHT_M);
            visit("building.min_height_m", b.min_height_m, Q_HEIGHT_M);
            visit("building.base_z_m", b.base_z_m, Q_HEIGHT_M);
        }
        for z in &self.landuse {
            for p in &z.ring {
                visit("landuse.ring.x", p.x, Q_POSITION_M);
                visit("landuse.ring.y", p.y, Q_POSITION_M);
                visit("landuse.ring.z", p.z, Q_HEIGHT_M);
            }
        }
        for s in &self.sites {
            visit("site.position.x", s.position.x, Q_POSITION_M);
            visit("site.position.y", s.position.y, Q_POSITION_M);
            visit("site.position.z", s.position.z, Q_HEIGHT_M);
            visit("site.antenna_height_m", s.antenna_height_m, Q_HEIGHT_M);
            visit("site.antenna_gain_dbi", s.antenna_gain_dbi, Q_DB);
        }
        for plan in &self.signals {
            visit("signal.cycle_s", plan.cycle_s, Q_TIME_S);
            visit("signal.offset_s", plan.offset_s, Q_TIME_S);
            for p in &plan.phases {
                visit("signal.phase.duration_s", p.duration_s, Q_TIME_S);
            }
            for h in &plan.heads {
                visit("signal.head.x", h.position.x, Q_POSITION_M);
                visit("signal.head.y", h.position.y, Q_POSITION_M);
                visit("signal.head.z", h.position.z, Q_HEIGHT_M);
            }
        }
        if let Some(t) = &self.terrain {
            visit("terrain.origin_x_m", t.origin_x_m, Q_POSITION_M);
            visit("terrain.origin_y_m", t.origin_y_m, Q_POSITION_M);
            visit("terrain.cell_x_m", t.cell_x_m, Q_POSITION_M);
            visit("terrain.cell_y_m", t.cell_y_m, Q_POSITION_M);
            for h in &t.heights_m {
                visit("terrain.heights_m", *h, Q_HEIGHT_M);
            }
        }
        if let Some(b) = self.provenance.source_bbox {
            visit("provenance.bbox.min_lat_deg", b.min_lat_deg, Q_DEGREES);
            visit("provenance.bbox.min_lon_deg", b.min_lon_deg, Q_DEGREES);
            visit("provenance.bbox.max_lat_deg", b.max_lat_deg, Q_DEGREES);
            visit("provenance.bbox.max_lon_deg", b.max_lon_deg, Q_DEGREES);
        }
        visit(
            "provenance.origin.lat_deg",
            self.provenance.origin.lat_deg,
            Q_DEGREES,
        );
        visit(
            "provenance.origin.lon_deg",
            self.provenance.origin.lon_deg,
            Q_DEGREES,
        );
        visit(
            "provenance.origin.alt_m",
            self.provenance.origin.alt_m,
            Q_HEIGHT_M,
        );
        // The transformation parameters. They are `f64`s serialised verbatim into every
        // artefact this crate writes, so D9 covers them exactly as it covers a
        // coordinate; leaving them out of the scan let a raw double
        // (`block_x_m = 120.000_000_123_456_7`) reach `world.json`, `world.v2xw` and the
        // `.vwb` provenance blob with `validate` reporting an empty offender list. This
        // is the one place the path is formatted rather than static: the *name* of a
        // free-form parameter is what a report needs, and there are a few dozen of them,
        // not a few hundred thousand.
        for t in &self.provenance.transformations {
            for (key, value, quantum) in t.float_params() {
                visit(
                    &format!("provenance.transformations.{}.{key}", t.name),
                    value,
                    quantum,
                );
            }
        }
    }
}

/// Assembles a [`World`], quantising, validating and hashing it once at the end.
///
/// Every importer and generator goes through this: it is the single place where a world
/// becomes immutable, on-grid and content-addressed.
#[derive(Debug)]
pub struct WorldBuilder {
    origin: GeoOrigin,
    bbox: Option<Bbox>,
    roads: Option<RoadNetwork>,
    buildings: Vec<Building>,
    terrain: Option<Terrain>,
    signals: Vec<SignalPlan>,
    sites: Vec<Site>,
    landuse: Vec<LanduseZone>,
    default_env: EnvClass,
    symbols: SymbolTable,
    provenance: Option<WorldProvenance>,
    index_options: IndexOptions,
    bbox_margin_m: f64,
}

impl WorldBuilder {
    /// A builder anchored at `origin`, with an empty road network.
    pub fn new(origin: GeoOrigin) -> Self {
        Self {
            origin,
            bbox: None,
            roads: None,
            buildings: Vec::new(),
            terrain: None,
            signals: Vec::new(),
            sites: Vec::new(),
            landuse: Vec::new(),
            default_env: EnvClass::Urban,
            symbols: SymbolTable::new(),
            provenance: None,
            index_options: IndexOptions::default(),
            bbox_margin_m: 0.0,
        }
    }

    /// Sets the road network.
    #[must_use]
    pub fn roads(mut self, roads: RoadNetwork) -> Self {
        self.roads = Some(roads);
        self
    }

    /// Sets the bounding box explicitly. Without one, [`WorldBuilder::build`] computes it
    /// from the geometry.
    #[must_use]
    pub fn bbox(mut self, bbox: Bbox) -> Self {
        self.bbox = Some(bbox);
        self
    }

    /// Grows the computed bounding box by this margin on every side. Ignored when the box
    /// was set explicitly.
    #[must_use]
    pub fn bbox_margin_m(mut self, margin_m: f64) -> Self {
        self.bbox_margin_m = margin_m;
        self
    }

    /// Sets the buildings (they must already be in dense id order).
    #[must_use]
    pub fn buildings(mut self, buildings: Vec<Building>) -> Self {
        self.buildings = buildings;
        self
    }

    /// Sets the terrain grid.
    #[must_use]
    pub fn terrain(mut self, terrain: Terrain) -> Self {
        self.terrain = Some(terrain);
        self
    }

    /// Sets the signal plans.
    #[must_use]
    pub fn signals(mut self, signals: Vec<SignalPlan>) -> Self {
        self.signals = signals;
        self
    }

    /// Sets the infrastructure sites.
    #[must_use]
    pub fn sites(mut self, sites: Vec<Site>) -> Self {
        self.sites = sites;
        self
    }

    /// Sets the land-use zones.
    #[must_use]
    pub fn landuse(mut self, landuse: Vec<LanduseZone>) -> Self {
        self.landuse = landuse;
        self
    }

    /// Sets the environment class outside every land-use zone.
    #[must_use]
    pub fn default_env(mut self, env: EnvClass) -> Self {
        self.default_env = env;
        self
    }

    /// Sets the symbol table (names must already be interned in it).
    #[must_use]
    pub fn symbols(mut self, symbols: SymbolTable) -> Self {
        self.symbols = symbols;
        self
    }

    /// Sets the provenance record. Required: a world without provenance cannot be built,
    /// because invariant I-W3 has nowhere to live.
    #[must_use]
    pub fn provenance(mut self, provenance: WorldProvenance) -> Self {
        self.provenance = Some(provenance);
        self
    }

    /// Sets how the lazily built spatial indices are sized.
    #[must_use]
    pub fn index_options(mut self, options: IndexOptions) -> Self {
        self.index_options = options;
        self
    }

    /// Interns a string in the world's symbol table.
    pub fn intern(&mut self, s: &str) -> SymbolId {
        self.symbols.intern(s)
    }

    /// Finishes the world: quantises what the component constructors did not, computes
    /// the bounding box if it was not given, validates, hashes and freezes.
    ///
    /// # Errors
    ///
    /// [`WorldError::InvalidParameter`] if no road network or no provenance was supplied,
    /// or whatever [`World::validate`] rejects.
    pub fn build(self) -> Result<World> {
        let roads = self.roads.ok_or_else(|| WorldError::InvalidParameter {
            parameter: "roads".to_string(),
            problem: "a world needs a road network".to_string(),
        })?;
        let mut provenance = self
            .provenance
            .ok_or_else(|| WorldError::InvalidParameter {
                parameter: "provenance".to_string(),
                problem: "a world needs a provenance record (invariant I-W3)".to_string(),
            })?;

        // Quantise what has no constructor of its own. Lanes, buildings, zones and the
        // terrain grid are quantised by theirs; junction geometry, signal plans and sites
        // are plain structs a caller fills in, so they are put on the grid here. Doing it
        // twice is harmless: quantisation is idempotent.
        let mut roads = roads;
        roads.quantise_in_place();
        let mut signals = self.signals;
        for plan in &mut signals {
            plan.cycle_s = quantise(plan.cycle_s, Q_TIME_S);
            plan.offset_s = quantise(plan.offset_s, Q_TIME_S);
            for phase in &mut plan.phases {
                phase.duration_s = quantise(phase.duration_s, Q_TIME_S);
            }
            for head in &mut plan.heads {
                head.position = quantise_vec3(head.position);
            }
        }
        let mut sites = self.sites;
        for site in &mut sites {
            site.position = quantise_vec3(site.position);
            site.antenna_height_m = quantise(site.antenna_height_m, Q_HEIGHT_M);
            site.antenna_gain_dbi = quantise(site.antenna_gain_dbi, Q_DB);
        }

        let bbox = match self.bbox {
            Some(b) => Bbox::new(quantise_vec3(b.min), quantise_vec3(b.max)),
            None => {
                let mut b = Bbox::empty();
                for lane in roads.lanes() {
                    for p in &lane.centreline {
                        b.include(*p);
                    }
                }
                for j in roads.junctions() {
                    b.include(j.position);
                    for p in &j.shape {
                        b.include(*p);
                    }
                }
                for c in roads.crossings() {
                    b.include(c.from);
                    b.include(c.to);
                }
                for building in &self.buildings {
                    for p in &building.footprint {
                        b.include(*p);
                        b.include(Vec3::new(p.x, p.y, building.roof_z_m()));
                    }
                }
                for z in &self.landuse {
                    for p in &z.ring {
                        b.include(*p);
                    }
                }
                for s in &sites {
                    b.include(s.antenna_position());
                }
                if b.is_empty() {
                    b = Bbox::new(Vec3::ZERO, Vec3::ZERO);
                }
                let b = b.expand(self.bbox_margin_m);
                Bbox::new(quantise_vec3(b.min), quantise_vec3(b.max))
            }
        };

        let mut world = World {
            origin: self.origin,
            bbox,
            roads,
            buildings: self.buildings,
            terrain: self.terrain,
            signals,
            sites,
            landuse: self.landuse,
            default_env: self.default_env,
            symbols: self.symbols,
            provenance: {
                provenance.origin = self.origin;
                provenance
            },
            content_hash: [0u8; 32],
            index_options: self.index_options,
            index: OnceLock::new(),
        };
        world.validate()?;
        world.content_hash = crate::hash::content_hash(&world);
        world.provenance.content_hash = world.content_hash;
        Ok(world)
    }
}

impl RoadNetwork {
    /// Puts junction geometry on the quantisation grid. Lane geometry is already there:
    /// [`Lane::new`] is the only way to make one.
    ///
    /// # Restoring the containment invariant
    ///
    /// An importer builds a junction's shape as the convex hull of its lane ends
    /// *together with its own position*, so the position starts inside its polygon. The
    /// position and the hull vertices are then quantised independently, and rounding them
    /// in opposite directions can move the position outside the polygon it is supposed to
    /// be the centre of — measured on the Phase 1 Manhattan world: 2 junctions of 3 421
    /// strictly outside, worst depth 2.43e-4 m. Because the hull is computed before the
    /// quantiser runs, nothing downstream could restore the property.
    ///
    /// So it is restored here, on the values that are actually stored: where the quantised
    /// position no longer lies in the quantised shape, the shape is re-hulled from its own
    /// quantised vertices *plus* the quantised position. Every input is then already on
    /// the grid, so the result is on the grid, and the position is a point of the hulled
    /// set and therefore inside it or on its boundary — exactly, not to within a quantum.
    /// [`World::validate`] asserts it afterwards
    /// ([`Junction::position_is_in_shape`]). A shape that degenerates once quantised
    /// (three collinear millimetre-grid points, say) becomes empty, which the model and
    /// the wire payload both allow for a junction with no area.
    fn quantise_in_place(&mut self) {
        for j in &mut self.junctions {
            j.position = quantise_vec3(j.position);
            for p in &mut j.shape {
                *p = quantise_vec3(*p);
            }
            if !j.shape.is_empty() && !j.position_is_in_shape() {
                let mut points = open_ring_points(&j.shape);
                points.push(j.position);
                j.shape = convex_hull_ring(&points);
            }
        }
        for c in &mut self.crossings {
            c.from = quantise_vec3(c.from);
            c.to = quantise_vec3(c.to);
            c.width_m = quantise(c.width_m, Q_POSITION_M);
        }
    }
}

/// Every field of a [`World`], as a plain struct — the form the readers rebuild from.
///
/// [`WorldBuilder`] is for *producing* a world: it computes the bounding box, quantises,
/// validates and hashes. This is for *restoring* one that was produced earlier, where the
/// bounding box and the content hash are part of the artefact and must come back exactly
/// as they were written rather than be recomputed.
#[derive(Debug, Clone, PartialEq)]
pub struct WorldParts {
    /// The geodetic anchor.
    pub origin: GeoOrigin,
    /// The extent, world-local metres.
    pub bbox: Bbox,
    /// The road graph.
    pub roads: RoadNetwork,
    /// Buildings, in id order.
    pub buildings: Vec<Building>,
    /// The terrain grid, if any.
    pub terrain: Option<Terrain>,
    /// Signal plans, in id order.
    pub signals: Vec<SignalPlan>,
    /// Sites, in id order.
    pub sites: Vec<Site>,
    /// Land-use zones, in id order.
    pub landuse: Vec<LanduseZone>,
    /// The environment outside every zone.
    pub default_env: EnvClass,
    /// The interned strings.
    pub symbols: SymbolTable,
    /// Where the world came from.
    pub provenance: WorldProvenance,
    /// The content hash as it was written.
    pub content_hash: [u8; 32],
    /// Index sizing.
    pub index_options: IndexOptions,
}

impl World {
    /// Rebuilds a world from its parts, validating it and checking its content hash.
    ///
    /// # Errors
    ///
    /// Whatever [`World::validate`] rejects, or [`WorldError::Invariant`] with `I-W2` if
    /// the stored content hash does not match the geometry — which is exactly the check
    /// that turns a corrupted or tampered world file into an error rather than into a run
    /// whose manifest lies about what was simulated.
    pub fn from_parts(parts: WorldParts) -> Result<World> {
        let world = World {
            origin: parts.origin,
            bbox: parts.bbox,
            roads: parts.roads,
            buildings: parts.buildings,
            terrain: parts.terrain,
            signals: parts.signals,
            sites: parts.sites,
            landuse: parts.landuse,
            default_env: parts.default_env,
            symbols: parts.symbols,
            provenance: parts.provenance,
            content_hash: parts.content_hash,
            index_options: parts.index_options,
            index: OnceLock::new(),
        };
        world.validate()?;
        let recomputed = crate::hash::content_hash(&world);
        if recomputed != world.content_hash {
            return Err(WorldError::Invariant {
                invariant: "I-W2",
                problem: format!(
                    "content hash {} does not match the geometry, which hashes to {}",
                    v2xw_core::hash::hex_encode(&world.content_hash),
                    v2xw_core::hash::hex_encode(&recomputed)
                ),
            });
        }
        Ok(world)
    }

    /// The world's parts, cloned out — the form the writers walk.
    pub fn to_parts(&self) -> WorldParts {
        WorldParts {
            origin: self.origin,
            bbox: self.bbox,
            roads: self.roads.clone(),
            buildings: self.buildings.clone(),
            terrain: self.terrain.clone(),
            signals: self.signals.clone(),
            sites: self.sites.clone(),
            landuse: self.landuse.clone(),
            default_env: self.default_env,
            symbols: self.symbols.clone(),
            provenance: self.provenance.clone(),
            content_hash: self.content_hash,
            index_options: self.index_options,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quant::{Q_POSITION_M, is_on_grid};

    /// The D7 Phase 1 Manhattan box: `-73.9900, 40.7440, -73.9680, 40.7620`.
    const MANHATTAN: GeoBbox = GeoBbox {
        min_lat_deg: 40.7440,
        min_lon_deg: -73.9900,
        max_lat_deg: 40.7620,
        max_lon_deg: -73.9680,
    };

    /// The Vincenty inverse solution on the WGS-84 ellipsoid: geodesic distance and
    /// initial azimuth between two geodetic points.
    ///
    /// Written out here so the projection's documented error bound is *checked* rather
    /// than asserted in prose. Every transcendental goes through [`v2xw_core::math`], the
    /// same as the rest of the engine.
    fn vincenty(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> (f64, f64) {
        const A: f64 = 6_378_137.0;
        const F: f64 = 1.0 / 298.257_223_563;
        let b = A * (1.0 - F);
        let l = (lon2 - lon1).to_radians();
        let u1 = math::atan((1.0 - F) * math::tan(lat1.to_radians()));
        let u2 = math::atan((1.0 - F) * math::tan(lat2.to_radians()));
        let (su1, cu1) = math::sin_cos(u1);
        let (su2, cu2) = math::sin_cos(u2);
        let mut lambda = l;
        let mut sin_sigma = 0.0;
        let mut cos_sigma = 1.0;
        let mut sigma = 0.0;
        let mut cos2_alpha = 1.0;
        let mut cos_2sigma_m = 0.0;
        for _ in 0..200 {
            let (sl, cl) = math::sin_cos(lambda);
            sin_sigma = math::hypot(cu2 * sl, cu1 * su2 - su1 * cu2 * cl);
            if sin_sigma == 0.0 {
                return (0.0, 0.0);
            }
            cos_sigma = su1 * su2 + cu1 * cu2 * cl;
            sigma = math::atan2(sin_sigma, cos_sigma);
            let sin_alpha = cu1 * cu2 * sl / sin_sigma;
            cos2_alpha = 1.0 - sin_alpha * sin_alpha;
            cos_2sigma_m = if cos2_alpha == 0.0 {
                0.0
            } else {
                cos_sigma - 2.0 * su1 * su2 / cos2_alpha
            };
            let c = F / 16.0 * cos2_alpha * (4.0 + F * (4.0 - 3.0 * cos2_alpha));
            let previous = lambda;
            lambda = l
                + (1.0 - c)
                    * F
                    * sin_alpha
                    * (sigma
                        + c * sin_sigma
                            * (cos_2sigma_m
                                + c * cos_sigma * (-1.0 + 2.0 * cos_2sigma_m * cos_2sigma_m)));
            if (lambda - previous).abs() < 1e-14 {
                break;
            }
        }
        let u_sq = cos2_alpha * (A * A - b * b) / (b * b);
        let a_big =
            1.0 + u_sq / 16384.0 * (4096.0 + u_sq * (-768.0 + u_sq * (320.0 - 175.0 * u_sq)));
        let b_big = u_sq / 1024.0 * (256.0 + u_sq * (-128.0 + u_sq * (74.0 - 47.0 * u_sq)));
        let d_sigma = b_big
            * sin_sigma
            * (cos_2sigma_m
                + b_big / 4.0
                    * (cos_sigma * (-1.0 + 2.0 * cos_2sigma_m * cos_2sigma_m)
                        - b_big / 6.0
                            * cos_2sigma_m
                            * (-3.0 + 4.0 * sin_sigma * sin_sigma)
                            * (-3.0 + 4.0 * cos_2sigma_m * cos_2sigma_m)));
        let distance = b * a_big * (sigma - d_sigma);
        let (sl, cl) = math::sin_cos(lambda);
        let azimuth = math::atan2(cu2 * sl, cu1 * su2 - su1 * cu2 * cl);
        (distance, azimuth)
    }

    /// The worst planar error of the projection over a square of side `extent_m`.
    fn worst_projection_error(origin: GeoOrigin, extent_m: f64, steps: u32) -> f64 {
        let p = Projection::new(origin);
        let dlat = extent_m / p.metres_per_degree_latitude();
        let dlon = extent_m / p.metres_per_degree_longitude();
        let mut worst: f64 = 0.0;
        for i in 0..=steps {
            for j in 0..=steps {
                let lat = origin.lat_deg + dlat * f64::from(j) / f64::from(steps);
                let lon = origin.lon_deg + dlon * f64::from(i) / f64::from(steps);
                let (x, y) = p.to_enu(lat, lon);
                let (d, az) = vincenty(origin.lat_deg, origin.lon_deg, lat, lon);
                let (true_x, true_y) = (d * math::sin(az), d * math::cos(az));
                worst = worst.max(math::hypot(x - true_x, y - true_y));
            }
        }
        worst
    }

    #[test]
    fn projection_round_trips() {
        let origin = GeoOrigin::new(MANHATTAN.min_lat_deg, MANHATTAN.min_lon_deg, 12.0);
        let p = Projection::new(origin);
        for (lat, lon) in [
            (40.7440, -73.9900),
            (40.7620, -73.9680),
            (40.7500, -73.9800),
        ] {
            let (x, y) = p.to_enu(lat, lon);
            let (lat2, lon2) = p.to_geodetic(x, y);
            assert!((lat - lat2).abs() < 1e-12, "lat {lat} came back as {lat2}");
            assert!((lon - lon2).abs() < 1e-12, "lon {lon} came back as {lon2}");
        }
        assert_eq!(p.to_enu(origin.lat_deg, origin.lon_deg), (0.0, 0.0));
        assert_eq!(p.to_altitude(3.0), 15.0);
        assert_eq!(p.origin(), origin);
        // The projection serialises as its origin and rebuilds the same scales.
        let text = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<Projection>(&text).unwrap(), p);
    }

    #[test]
    fn projection_scale_factors_match_the_series() {
        let p = Projection::new(GeoOrigin::new(40.7440, -73.9900, 0.0));
        // The published values of the truncated series at 40.744° N.
        assert!((p.metres_per_degree_latitude() - 111_048.934).abs() < 0.01);
        assert!((p.metres_per_degree_longitude() - 84_459.950).abs() < 0.01);
        // At the equator both cosines are 1, so the longitude scale is the leading
        // constant less the third-harmonic term.
        let equator = Projection::new(GeoOrigin::new(0.0, 0.0, 0.0));
        assert!((equator.metres_per_degree_longitude() - (111_412.84 - 93.5)).abs() < 1e-6);
    }

    /// The error bound in [`Projection`]'s documentation, checked against a geodesic
    /// reference rather than taken on trust.
    #[test]
    fn projection_error_matches_the_documented_bound() {
        let origin = GeoOrigin::new(MANHATTAN.min_lat_deg, MANHATTAN.min_lon_deg, 0.0);
        let p = Projection::new(origin);

        // Over the D7 Manhattan box: documented as 0.56 m at the far corner.
        let mut worst: f64 = 0.0;
        for i in 0..=20 {
            for j in 0..=20 {
                let lat = MANHATTAN.min_lat_deg
                    + (MANHATTAN.max_lat_deg - MANHATTAN.min_lat_deg) * f64::from(j) / 20.0;
                let lon = MANHATTAN.min_lon_deg
                    + (MANHATTAN.max_lon_deg - MANHATTAN.min_lon_deg) * f64::from(i) / 20.0;
                let (x, y) = p.to_enu(lat, lon);
                let (d, az) = vincenty(origin.lat_deg, origin.lon_deg, lat, lon);
                worst = worst.max(math::hypot(x - d * math::sin(az), y - d * math::cos(az)));
            }
        }
        assert!(
            (0.45..0.65).contains(&worst),
            "the documented 0.56 m over the Manhattan box is now {worst} m"
        );

        // And the quadratic growth the documentation tabulates.
        let one_km = worst_projection_error(origin, 1_000.0, 10);
        let two_km = worst_projection_error(origin, 2_000.0, 10);
        let four_km = worst_projection_error(origin, 4_000.0, 10);
        assert!((0.10..0.20).contains(&one_km), "1 km: {one_km} m");
        assert!((0.50..0.70).contains(&two_km), "2 km: {two_km} m");
        assert!((2.2..2.7).contains(&four_km), "4 km: {four_km} m");
        assert!(
            four_km / two_km > 3.5 && four_km / two_km < 4.5,
            "the error should grow with the square of the extent"
        );
    }

    #[test]
    fn geo_origin_and_bbox_quantise_and_order() {
        let o = GeoOrigin::new(40.744_000_049, -73.990_000_051, 1.000_4);
        assert_eq!(o.lat_deg, 40.744);
        assert_eq!(o.lon_deg, -73.990_000_1);
        assert_eq!(o.alt_m, 1.0);
        let b = GeoBbox::new(40.762, -73.968, 40.744, -73.990);
        assert_eq!(b.south_west(), (40.744, -73.99));
        assert_eq!(b.to_lon_lat_array(), [-73.99, 40.744, -73.968, 40.762]);
        let (lat, lon) = b.centre();
        assert!((lat - 40.753).abs() < 1e-9 && (lon + 73.979).abs() < 1e-9);
    }

    #[test]
    fn symbol_table_interns_in_insertion_order() {
        let mut t = SymbolTable::new();
        assert_eq!(t.len(), 1);
        assert!(t.is_empty());
        let a = t.intern("Broadway");
        let b = t.intern("Wall Street");
        assert_eq!(a, SymbolId::new(1));
        assert_eq!(b, SymbolId::new(2));
        assert_eq!(t.intern("Broadway"), a, "interning is idempotent");
        assert_eq!(t.resolve(a), "Broadway");
        assert_eq!(t.resolve(SymbolId::new(999)), "");
        assert_eq!(t.resolve_optional(None), "");
        assert_eq!(t.intern_optional(""), None);
        assert_eq!(t.get("Wall Street"), Some(b));
        assert_eq!(t.get("nowhere"), None);
        assert!(!t.is_empty());

        // The serialised form is the string list, and a list that lost its empty first
        // entry is repaired rather than rejected.
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(json, r#"["","Broadway","Wall Street"]"#);
        assert_eq!(serde_json::from_str::<SymbolTable>(&json).unwrap(), t);
        let repaired: SymbolTable = serde_json::from_str(r#"["one","two"]"#).unwrap();
        assert_eq!(repaired.strings(), ["", "one", "two"]);
    }

    #[test]
    fn class_mask_matches_the_wire_bits() {
        assert_eq!(ClassMask::CAR.bits(), 1);
        assert_eq!(ClassMask::TRUCK.bits(), 2);
        assert_eq!(ClassMask::BUS.bits(), 4);
        assert_eq!(ClassMask::MOTO.bits(), 8);
        assert_eq!(ClassMask::BICYCLE.bits(), 16);
        assert_eq!(ClassMask::PEDESTRIAN.bits(), 32);
        assert_eq!(ClassMask::EMERGENCY.bits(), 64);
        assert_eq!(ClassMask::RAIL.bits(), 128);
        assert_eq!(ClassMask::ALL.bits(), 255);
        assert!(ClassMask::NONE.is_empty());
        assert_eq!(ClassMask::from_bits(0xFF00), ClassMask::NONE);

        let m = ClassMask::MOTOR_TRAFFIC;
        assert!(m.contains_any(ClassMask::CAR));
        assert!(m.contains_all(ClassMask::CAR.union(ClassMask::BUS)));
        assert!(!m.contains_any(ClassMask::BICYCLE));
        assert!(!m.difference(ClassMask::CAR).contains_any(ClassMask::CAR));
        assert_eq!(m.intersection(ClassMask::ALL), m);
        assert_eq!(
            m.names(),
            ["car", "truck", "bus", "moto", "emergency"],
            "names come out in bit order"
        );
        assert_eq!(
            ClassMask::from_names(["car", "rail", "nonsense"]).bits(),
            129
        );
        assert_eq!(m.to_string(), "car+truck+bus+moto+emergency");
        assert_eq!(ClassMask::NONE.to_string(), "none");
    }

    fn straight_lane() -> Lane {
        Lane::new(
            LaneId::new(0),
            EdgeId::new(0),
            None,
            0,
            LaneKind::Driving,
            [
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(10.0, 0.0, 0.0),
                Vec3::new(10.0, 10.0, 1.0),
            ],
            3.5,
            13.89,
            ClassMask::MOTOR_TRAFFIC,
        )
        .unwrap()
    }

    #[test]
    fn lane_geometry() {
        let lane = straight_lane();
        assert_eq!(lane.point_count(), 3);
        assert_eq!(lane.start(), Vec3::new(0.0, 0.0, 0.0));
        assert_eq!(lane.end(), Vec3::new(10.0, 10.0, 1.0));
        assert_eq!(lane.cumulative, vec![0.0, 10.0, 20.05]);
        assert_eq!(lane.length_m, 20.05);

        assert_eq!(lane.segment_at(-1.0), 0);
        assert_eq!(lane.segment_at(0.0), 0);
        assert_eq!(lane.segment_at(5.0), 0);
        assert_eq!(
            lane.segment_at(10.0),
            1,
            "a vertex belongs to the segment it starts"
        );
        assert_eq!(lane.segment_at(1e9), 1);

        assert_eq!(lane.point_at(0.0), Vec3::new(0.0, 0.0, 0.0));
        assert_eq!(lane.point_at(5.0), Vec3::new(5.0, 0.0, 0.0));
        assert_eq!(lane.point_at(10.0), Vec3::new(10.0, 0.0, 0.0));
        assert_eq!(lane.point_at(1e9), lane.end());
        assert_eq!(lane.heading_at(1.0), 0.0, "east is heading 0");
        assert!(
            (lane.heading_at(12.0) - core::f64::consts::FRAC_PI_2).abs() < 1e-12,
            "north is +π/2"
        );
        let (p, h) = lane.pose_at(5.0);
        assert_eq!((p, h), (Vec3::new(5.0, 0.0, 0.0), 0.0));

        // Positive offsets go to the left of travel: travelling east, left is north.
        assert_eq!(lane.offset_point(5.0, 2.0), Vec3::new(5.0, 2.0, 0.0));
        assert_eq!(lane.offset_point(5.0, -2.0), Vec3::new(5.0, -2.0, 0.0));
        assert_eq!(lane.offset_point(5.0, 0.0), lane.point_at(5.0));

        let hit = lane.project_point(Vec3::new(5.0, 2.0, 99.0));
        assert_eq!(hit.s_m, 5.0);
        assert_eq!(hit.d_m, 2.0);
        assert_eq!(hit.distance_m, 2.0);
        assert_eq!(hit.point, Vec3::new(5.0, 0.0, 0.0));
        assert_eq!(hit.segment, 0);
        assert!(lane.admits(ClassMask::CAR));
        assert!(!lane.admits(ClassMask::BICYCLE));
    }

    #[test]
    fn lane_rejects_geometry_it_cannot_use() {
        let one_point = Lane::new(
            LaneId::new(1),
            EdgeId::new(0),
            None,
            0,
            LaneKind::Driving,
            [Vec3::ZERO],
            3.5,
            10.0,
            ClassMask::ALL,
        );
        assert!(matches!(
            one_point,
            Err(WorldError::ShortCentreline { points: 1, .. })
        ));

        let too_close = Lane::new(
            LaneId::new(1),
            EdgeId::new(0),
            None,
            0,
            LaneKind::Driving,
            [Vec3::ZERO, Vec3::new(0.000_4, 0.0, 0.0)],
            3.5,
            10.0,
            ClassMask::ALL,
        );
        assert!(matches!(
            too_close,
            Err(WorldError::DegenerateSegment { .. })
        ));

        let infinite = Lane::new(
            LaneId::new(1),
            EdgeId::new(0),
            None,
            0,
            LaneKind::Driving,
            [Vec3::ZERO, Vec3::new(f64::INFINITY, 0.0, 0.0)],
            3.5,
            10.0,
            ClassMask::ALL,
        );
        assert!(matches!(infinite, Err(WorldError::NonFinite { .. })));
    }

    #[test]
    fn lane_geometry_is_quantised_on_construction() {
        let lane = Lane::new(
            LaneId::new(0),
            EdgeId::new(0),
            None,
            0,
            LaneKind::Driving,
            [
                Vec3::new(0.000_123, 0.0, 0.0),
                Vec3::new(9.999_777, 0.123_456, 0.9),
            ],
            3.499_9,
            13.888_888,
            ClassMask::ALL,
        )
        .unwrap();
        assert_eq!(
            lane.centreline[0].x, 0.0,
            "0.000123 m rounds to the nearest mm"
        );
        assert_eq!(lane.centreline[1], Vec3::new(10.0, 0.123, 0.9));
        assert_eq!(lane.width_m, 3.5);
        assert_eq!(lane.speed_limit_mps, 13.889);
        assert!(is_on_grid(lane.length_m, Q_POSITION_M));
        assert!(lane.cumulative.iter().all(|c| is_on_grid(*c, Q_POSITION_M)));
    }

    #[test]
    fn rdp_keeps_the_shape_and_both_ends() {
        let line = [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(5.0, 0.01, 1.0),
            Vec3::new(10.0, 0.0, 2.0),
            Vec3::new(10.0, 10.0, 3.0),
        ];
        // A tolerance above the wobble drops the middle point.
        let kept = simplify_rdp(&line, 0.5);
        assert_eq!(kept, vec![line[0], line[2], line[3]]);
        // A tolerance below it keeps everything.
        assert_eq!(simplify_rdp(&line, 0.001), line.to_vec());
        // Degenerate inputs are returned untouched.
        assert_eq!(simplify_rdp(&line, 0.0), line.to_vec());
        assert_eq!(simplify_rdp(&line, f64::NAN), line.to_vec());
        assert_eq!(simplify_rdp(&line[..2], 1.0), line[..2].to_vec());
        assert_eq!(simplify_rdp(&[], 1.0), Vec::<Vec3>::new());
        // A corner is never dropped, however coarse the tolerance.
        let corner = simplify_rdp(&line, 1_000.0);
        assert_eq!(corner, vec![line[0], line[3]]);
        // Height is carried, not considered: a purely vertical wobble survives.
        let ramp = [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(5.0, 0.0, 9.0),
            Vec3::new(10.0, 0.0, 0.0),
        ];
        assert_eq!(simplify_rdp(&ramp, 0.5), vec![ramp[0], ramp[2]]);
    }

    #[test]
    fn rings_are_wound_and_closed() {
        // Given clockwise, the outer ring comes back counter-clockwise and closed.
        let b = Building::new(
            BuildingId::new(0),
            [
                Vec3::new_2d(0.0, 0.0),
                Vec3::new_2d(0.0, 10.0),
                Vec3::new_2d(10.0, 10.0),
                Vec3::new_2d(10.0, 0.0),
            ],
            vec![vec![
                Vec3::new_2d(2.0, 2.0),
                Vec3::new_2d(4.0, 2.0),
                Vec3::new_2d(4.0, 4.0),
                Vec3::new_2d(2.0, 4.0),
            ]],
            21.5,
            0.0,
            MaterialClass::Concrete,
            HeightSource::Tagged,
        )
        .unwrap();
        assert_eq!(b.footprint.len(), 5);
        assert_eq!(b.footprint[0], b.footprint[4], "closed");
        assert!(ring_signed_area_2x(&b.footprint) > 0.0, "counter-clockwise");
        assert_eq!(b.open_ring().len(), 4);
        assert_eq!(b.holes.len(), 1);
        assert!(
            ring_signed_area_2x(&b.holes[0]) < 0.0,
            "holes wind the other way"
        );
        assert_eq!(b.roof_z_m(), 21.5);
        assert!(b.contains_2d(Vec3::new_2d(5.0, 8.0)));
        assert!(!b.contains_2d(Vec3::new_2d(3.0, 3.0)), "inside the hole");
        assert!(!b.contains_2d(Vec3::new_2d(-1.0, 5.0)));
        assert_eq!(b.bbox().max.z, 21.5);

        let too_small = Building::new(
            BuildingId::new(1),
            [Vec3::ZERO, Vec3::new_2d(1.0, 0.0)],
            Vec::new(),
            10.0,
            0.0,
            MaterialClass::Unknown,
            HeightSource::Defaulted,
        );
        assert!(matches!(too_small, Err(WorldError::ShortRing { .. })));
    }

    #[test]
    fn terrain_samples_bilinearly() {
        let t = Terrain::new(
            0.0,
            0.0,
            10.0,
            10.0,
            2,
            2,
            vec![0.0, 10.0, 20.0, 30.0],
            Interpolation::Bilinear,
        )
        .unwrap();
        assert_eq!(t.sample_at(0, 0), Some(0.0));
        assert_eq!(t.sample_at(1, 1), Some(30.0));
        assert_eq!(t.sample_at(2, 0), None);
        assert_eq!(t.height_at(0.0, 0.0), Some(0.0));
        assert_eq!(t.height_at(10.0, 0.0), Some(10.0));
        assert_eq!(t.height_at(5.0, 0.0), Some(5.0));
        assert_eq!(t.height_at(5.0, 5.0), Some(15.0));
        assert_eq!(t.height_at(-0.1, 0.0), None);
        assert_eq!(t.height_at(0.0, 10.1), None);
        assert_eq!(t.extent().max, Vec3::new(10.0, 10.0, 0.0));

        let nearest = Terrain::new(
            0.0,
            0.0,
            10.0,
            10.0,
            2,
            2,
            vec![0.0, 10.0, 20.0, 30.0],
            Interpolation::Nearest,
        )
        .unwrap();
        assert_eq!(nearest.height_at(4.0, 4.0), Some(0.0));
        assert_eq!(nearest.height_at(6.0, 6.0), Some(30.0));

        assert!(
            Terrain::new(
                0.0,
                0.0,
                10.0,
                10.0,
                1,
                2,
                vec![0.0, 1.0],
                Interpolation::Bilinear
            )
            .is_err()
        );
        assert!(
            Terrain::new(
                0.0,
                0.0,
                0.0,
                10.0,
                2,
                2,
                vec![0.0; 4],
                Interpolation::Bilinear
            )
            .is_err()
        );
        assert!(
            Terrain::new(
                0.0,
                0.0,
                10.0,
                10.0,
                2,
                2,
                vec![0.0; 3],
                Interpolation::Bilinear
            )
            .is_err()
        );
    }

    #[test]
    fn signal_plan_finds_its_phase() {
        let plan = SignalPlan {
            id: SignalId::new(0),
            junction: JunctionId::new(0),
            cycle_s: 60.0,
            offset_s: 5.0,
            controlled: vec![LaneId::new(0)],
            phases: vec![
                SignalPhase {
                    duration_s: 27.0,
                    states: vec![SignalState::Green],
                    name: None,
                },
                SignalPhase {
                    duration_s: 3.0,
                    states: vec![SignalState::Amber],
                    name: None,
                },
                SignalPhase {
                    duration_s: 30.0,
                    states: vec![SignalState::Red],
                    name: None,
                },
            ],
            heads: Vec::new(),
        };
        assert_eq!(plan.total_phase_duration_s(), 60.0);
        assert_eq!(plan.phase_at(5.0), Some((0, 0.0)));
        assert_eq!(plan.phase_at(31.0), Some((0, 26.0)));
        assert_eq!(plan.phase_at(33.0), Some((1, 1.0)));
        assert_eq!(plan.phase_at(40.0), Some((2, 5.0)));
        assert_eq!(plan.phase_at(65.0), Some((0, 0.0)), "the cycle wraps");
        assert_eq!(plan.phase_at(0.0), Some((2, 25.0)), "before the offset");
        assert_eq!(plan.states_at(5.0), Some(&[SignalState::Green][..]));
        assert!(SignalState::Green.permits_entry());
        assert!(SignalState::GreenYield.permits_entry());
        assert!(!SignalState::Amber.permits_entry());
        assert_eq!(SignalState::Red.sumo_letter(), 'r');
    }

    #[test]
    fn conflict_matrix_stores_both_directions() {
        let mut m = ConflictMatrix::new(70); // more than one 64-bit word per row
        assert_eq!(m.len(), 70);
        assert!(!m.is_empty());
        m.set_foe(3, 65, true);
        assert!(m.is_foe(3, 65) && m.is_foe(65, 3), "foes are symmetric");
        m.set_response(3, 65, true);
        assert!(m.must_yield(3, 65));
        assert!(!m.must_yield(65, 3), "responses are not");
        assert_eq!(m.foes_of(3), vec![65]);
        m.set_foe(3, 65, false);
        assert!(!m.is_foe(3, 65));
        // Out of range is ignored, not a panic.
        m.set_foe(0, 999, true);
        assert!(!m.is_foe(0, 999));
        assert!(ConflictMatrix::new(0).is_empty());

        let (foes, response) = m.raw();
        let rebuilt =
            ConflictMatrix::from_raw(70, foes.to_vec(), response.to_vec()).expect("round trip");
        assert_eq!(rebuilt, m);
        assert!(ConflictMatrix::from_raw(70, vec![0; 3], vec![0; 3]).is_none());
    }

    #[test]
    fn turn_directions_come_from_heading_changes() {
        use core::f64::consts::PI;
        assert_eq!(
            TurnDirection::from_heading_change(0.0),
            TurnDirection::Straight
        );
        assert_eq!(
            TurnDirection::from_heading_change(PI / 2.0),
            TurnDirection::Left
        );
        assert_eq!(
            TurnDirection::from_heading_change(-PI / 2.0),
            TurnDirection::Right
        );
        assert_eq!(
            TurnDirection::from_heading_change(PI / 4.0),
            TurnDirection::SlightLeft
        );
        assert_eq!(
            TurnDirection::from_heading_change(-PI / 4.0),
            TurnDirection::SlightRight
        );
        assert_eq!(TurnDirection::from_heading_change(PI), TurnDirection::UTurn);
        // A heading change is wrapped first, so 3π/2 to the left is a right turn.
        assert_eq!(
            TurnDirection::from_heading_change(3.0 * PI / 2.0),
            TurnDirection::Right
        );
        assert!(TurnDirection::Left.crosses_opposing_traffic());
        assert!(TurnDirection::UTurn.crosses_opposing_traffic());
        assert!(!TurnDirection::Right.crosses_opposing_traffic());
        assert_eq!(TurnDirection::Straight.sumo_code(), 's');
        assert_eq!(normalise_angle(0.0), 0.0);
        assert!((normalise_angle(3.0 * PI) - PI).abs() < 1e-12);
        assert!((normalise_angle(-3.0 * PI) - PI).abs() < 1e-12);
    }

    #[test]
    fn wire_codes_round_trip() {
        for kind in LaneKind::ALL {
            assert_eq!(LaneKind::from_wire_code(kind.wire_code()), Some(kind));
            assert!(!kind.wire_name().is_empty());
        }
        assert_eq!(LaneKind::from_wire_code(7), None);
        assert_eq!(LaneKind::Driving.wire_code(), 0);
        assert_eq!(LaneKind::Internal.wire_name(), "junction-internal");
        assert!(LaneKind::Driving.is_motorised());
        assert!(!LaneKind::Sidewalk.is_motorised());

        for m in [
            MaterialClass::Unknown,
            MaterialClass::Concrete,
            MaterialClass::Brick,
            MaterialClass::Glass,
            MaterialClass::Wood,
            MaterialClass::Metal,
        ] {
            assert_eq!(MaterialClass::from_wire_code(m.wire_code()), Some(m));
        }
        assert_eq!(MaterialClass::from_wire_code(6), None);
        for l in [LodHint::Box, LodHint::BoxRoof, LodHint::Detailed] {
            assert_eq!(LodHint::from_wire_code(l.wire_code()), Some(l));
        }
        for c in [
            LanduseClass::Urban,
            LanduseClass::Suburban,
            LanduseClass::Rural,
            LanduseClass::Highway,
            LanduseClass::Water,
            LanduseClass::Park,
            LanduseClass::Industrial,
        ] {
            assert_eq!(LanduseClass::from_wire_code(c.wire_code()), Some(c));
            assert!(!c.wire_name().is_empty());
        }
        // The specification's numbering: 4 is water, 5 is park (§4.5).
        assert_eq!(LanduseClass::Water.wire_code(), 4);
        assert_eq!(LanduseClass::Park.wire_code(), 5);
        assert_eq!(LanduseClass::Urban.default_env(), EnvClass::Urban);
        assert_eq!(LanduseClass::Park.default_env(), EnvClass::Rural);

        assert_eq!(JunctionControl::Uncontrolled.wire_code(), 0);
        assert_eq!(JunctionControl::Priority.wire_code(), 1);
        assert_eq!(
            JunctionControl::Signalised {
                plan: SignalId::new(3)
            }
            .wire_code(),
            2
        );
        assert_eq!(
            JunctionControl::Signalised {
                plan: SignalId::new(3)
            }
            .plan(),
            Some(SignalId::new(3))
        );
        assert_eq!(JunctionControl::Stop.plan(), None);
        assert_eq!(JunctionControl::Roundabout.wire_name(), "roundabout");
        assert_eq!(SiteKind::Cell.wire_code(), 1);
        assert_eq!(SignalHeadKind::Pedestrian.wire_name(), "pedestrian");
        assert_eq!(EnvClass::Highway.label(), "highway");
        assert_eq!(HeightSource::FromLevels.label(), "from-levels");
        assert_eq!(RoadClass::Motorway.label(), "motorway");
        assert_eq!(Interpolation::Bilinear.label(), "bilinear");
        assert_eq!(WorldSourceKind::Osm.label(), "osm");
    }

    #[test]
    fn world_ids_display_with_their_prefix() {
        assert_eq!(CrossingId::new(1).to_string(), "x1");
        assert_eq!(SiteId::new(2).to_string(), "st2");
        assert_eq!(ZoneId::new(3).to_string(), "z3");
        assert_eq!(SymbolId::new(4).to_string(), "s4");
        assert_eq!(SymbolId::new(4).index(), 4);
        assert_eq!(SymbolId::new(4).as_usize(), 4);
    }

    #[test]
    fn transformations_render_for_the_wire() {
        let plain = Transformation::new("local-tangent-plane");
        assert_eq!(plain.to_wire_string(), "local-tangent-plane");
        let with_params = Transformation::new("simplify")
            .with("tolerance_m", 0.25)
            .with("algorithm", "rdp");
        assert_eq!(
            with_params.to_wire_string(),
            "simplify:algorithm=rdp,tolerance_m=0.25",
            "parameters are ordered by key, so the record is byte-stable"
        );
    }

    #[test]
    fn provenance_summarises_licences_and_attribution() {
        let mut p = WorldProvenance::new(
            WorldSourceKind::Osm,
            "manhattan",
            "2026-09-18T00:00:00Z",
            GeoOrigin::NULL_ISLAND,
        );
        assert_eq!(p.wire_licence(), "");
        p.layers.push(LayerLicence::new("roads", "ODbL-1.0"));
        p.layers.push(LayerLicence::new("buildings", "ODbL-1.0"));
        assert_eq!(p.wire_licence(), "ODbL-1.0");
        p.layers.push(LayerLicence::with_attribution(
            "terrain",
            "Copernicus",
            "© DLR e.V. 2010-2014",
        ));
        assert_eq!(
            p.wire_licence(),
            "roads=ODbL-1.0; buildings=ODbL-1.0; terrain=Copernicus"
        );
        assert_eq!(p.required_attributions(), ["© DLR e.V. 2010-2014"]);
        p.record_dropped("building_holes", 40);
        p.record_dropped("building_holes", 1);
        assert_eq!(p.dropped["building_holes"], 41);

        let wire = p.to_wire_json();
        assert_eq!(wire["source"], "osm");
        assert_eq!(wire["imported_at"], "2026-09-18T00:00:00Z");
        assert_eq!(wire["dropped"]["building_holes"], 41);
        assert_eq!(wire["attribution"][0], "© DLR e.V. 2010-2014");
    }

    /// The loop form [`normalise_angle`] used to be, with an iteration cap so that a test
    /// can *observe* it failing to terminate instead of hanging the suite.
    fn loop_normalise(mut a: f64, cap: u32) -> Option<f64> {
        const PI: f64 = core::f64::consts::PI;
        const TAU: f64 = core::f64::consts::TAU;
        let mut steps = 0u32;
        while a > PI {
            a -= TAU;
            steps += 1;
            if steps > cap {
                return None;
            }
        }
        while a <= -PI {
            a += TAU;
            steps += 1;
            if steps > cap {
                return None;
            }
        }
        Some(a)
    }

    /// R9: `normalise_angle` is now a remainder rather than a loop. Two things have to
    /// hold: it returns **the same bits** as the loop over the range every caller uses, so
    /// no geometry moves; and it terminates for every finite input, which the loop did
    /// not.
    #[test]
    fn normalise_angle_matches_the_loop_it_replaced_and_always_terminates() {
        const PI: f64 = core::f64::consts::PI;
        const TAU: f64 = core::f64::consts::TAU;

        // Bit-for-bit over |a| ≤ 2τ. A caller passes the difference of two `heading_2d()`
        // results, which is inside (-τ, τ), so this covers every caller twice over. Both
        // forms are exact there — the loop's single add or subtract is exact by Sterbenz's
        // lemma, and `%` is exact always — so "the same real number" means "the same
        // bits".
        let mut a = -2.0 * TAU;
        let step = TAU / 5000.0;
        let mut checked = 0u32;
        while a <= 2.0 * TAU {
            let want = loop_normalise(a, 10).expect("the loop needs at most 2 steps here");
            let got = normalise_angle(a);
            assert_eq!(
                got.to_bits(),
                want.to_bits(),
                "a = {a}: loop gave {want}, remainder gave {got}"
            );
            assert!(got > -PI && got <= PI, "a = {a} left the range: {got}");
            a += step;
            checked += 1;
        }
        assert!(
            checked >= 20_000,
            "the sweep must actually sweep: {checked}"
        );

        // The exact endpoints, where ties matter.
        for (input, want) in [
            (0.0, 0.0),
            (PI, PI),
            (-PI, PI),
            (TAU, 0.0),
            (-TAU, 0.0),
            (3.0 * PI, PI),
            (-3.0 * PI, PI),
        ] {
            assert_eq!(normalise_angle(input).to_bits(), want.to_bits(), "{input}");
        }

        // Large finite inputs. The loop cannot do these: at 1e9 it needs more than
        // 1.5e8 iterations and at f64::MAX the subtraction is absorbed and it never
        // returns at all, so the reference is asked for a bounded number of steps and is
        // expected to give up.
        for huge in [1e9, 1e17, 1e300, f64::MAX] {
            for signed in [huge, -huge] {
                assert!(
                    loop_normalise(signed, 10_000_000).is_none(),
                    "the loop form should not have terminated for {signed}"
                );
                let got = normalise_angle(signed);
                assert!(
                    got > -PI && got <= PI,
                    "normalise_angle({signed}) = {got} is outside (-π, π]"
                );
            }
        }

        // And it terminates *promptly*, measured rather than asserted: a thread with a
        // five-second budget. Before the change this test failed by timing out rather
        // than hanging the whole suite, which is why it is written this way.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let worst = [1e9, 1e17, 1e300, f64::MAX, -1e9, -f64::MAX]
                .map(normalise_angle)
                .iter()
                .fold(0.0f64, |m, v| m.max(v.abs()));
            let _ = tx.send(worst);
        });
        let worst = rx
            .recv_timeout(core::time::Duration::from_secs(5))
            .expect("normalise_angle must terminate for every finite input");
        assert!(worst <= PI, "worst = {worst}");

        // Non-finite: NaN passes through, an infinity has no angle.
        assert!(normalise_angle(f64::NAN).is_nan());
        assert!(normalise_angle(f64::INFINITY).is_nan());
        assert!(normalise_angle(f64::NEG_INFINITY).is_nan());
    }

    /// R10: a ring whose signed area is exactly zero has no winding, so the winding test
    /// left it in whatever order the source gave it. A bow-tie listed clockwise and the
    /// same bow-tie listed counter-clockwise therefore produced two different buildings
    /// out of one shape, while vwp-v1 §4.4 declares payload rings counter-clockwise.
    /// Neither order is counter-clockwise here, so the ring is made **canonical** instead.
    #[test]
    fn a_degenerate_ring_is_canonical_whatever_order_it_arrives_in() {
        // A bow-tie: the diagonals cross, and the two lobes cancel exactly.
        let bow_tie = [
            Vec3::new_2d(0.0, 0.0),
            Vec3::new_2d(10.0, 10.0),
            Vec3::new_2d(10.0, 0.0),
            Vec3::new_2d(0.0, 10.0),
        ];
        assert_eq!(
            ring_signed_area_2x(&close_ring(bow_tie.to_vec())),
            0.0,
            "the fixture must really be degenerate, or the test proves nothing"
        );

        let build = |ring: Vec<Vec3>| {
            Building::new(
                BuildingId::new(0),
                ring,
                Vec::new(),
                20.0,
                0.0,
                MaterialClass::Unknown,
                HeightSource::Defaulted,
            )
            .expect("a bow-tie is still four distinct points")
        };

        // Every reading of the same cycle: each rotation, in both directions, closed and
        // open. All of them must produce the identical stored footprint.
        let reference = build(bow_tie.to_vec()).footprint;
        for start in 0..bow_tie.len() {
            for reverse in [false, true] {
                let mut ring: Vec<Vec3> = (0..bow_tie.len())
                    .map(|k| bow_tie[(start + k) % bow_tie.len()])
                    .collect();
                if reverse {
                    ring.reverse();
                }
                assert_eq!(
                    build(ring.clone()).footprint,
                    reference,
                    "start {start}, reverse {reverse}"
                );
                assert_eq!(
                    build(close_ring(ring)).footprint,
                    reference,
                    "closed: start {start}, reverse {reverse}"
                );
            }
        }
        assert_eq!(reference.len(), 5, "still closed");
        assert_eq!(reference[0], reference[4]);
        assert_eq!(ring_signed_area_2x(&reference), 0.0, "still no area");

        // An ordinary ring is still wound counter-clockwise, and is *not* rotated: the
        // canonical order is for the degenerate case only.
        let square = vec![
            Vec3::new_2d(1.0, 1.0),
            Vec3::new_2d(1.0, 5.0),
            Vec3::new_2d(5.0, 5.0),
            Vec3::new_2d(5.0, 1.0),
        ];
        let wound = build(square.clone()).footprint;
        assert!(ring_signed_area_2x(&wound) > 0.0);
        assert_eq!(
            &wound[..4],
            &[square[3], square[2], square[1], square[0]],
            "an ordinary ring is reversed if its winding is wrong, and never rotated"
        );
    }

    /// R11: quantising a junction's position and its shape independently could leave the
    /// position outside its own polygon. `quantise_in_place` re-establishes the invariant
    /// on the values that are actually stored, and `position_is_in_shape` is the assertion
    /// `World::validate` makes.
    ///
    /// The real case is a sub-millimetre coincidence — the worst Manhattan junction was
    /// 2.43e-4 m outside — and searching for one in a test would be a search for a
    /// rounding accident. The repair is the same code at any distance, so the fixture puts
    /// the position a metre outside instead, which is deterministic and unambiguous.
    #[test]
    fn quantising_a_junction_puts_its_position_back_inside_its_shape() {
        let shape = close_ring(vec![
            Vec3::new_2d(5.0, 5.0),
            Vec3::new_2d(6.0, 5.0),
            Vec3::new_2d(6.0, 6.0),
            Vec3::new_2d(5.0, 6.0),
        ]);
        let junction = Junction {
            id: JunctionId::new(0),
            position: Vec3::new(4.0, 5.5, 0.0),
            shape: shape.clone(),
            incoming: Vec::new(),
            outgoing: Vec::new(),
            internal: Vec::new(),
            control: JunctionControl::Uncontrolled,
            conflicts: ConflictMatrix::new(0),
            name: None,
        };
        let mut net = RoadNetwork::new(
            Vec::new(),
            Vec::new(),
            vec![junction],
            Vec::new(),
            Vec::new(),
        )
        .expect("one junction and nothing else is a valid network");
        assert!(
            !net.junctions()[0].position_is_in_shape(),
            "the fixture must start broken"
        );

        net.quantise_in_place();

        let j = &net.junctions()[0];
        assert!(
            j.position_is_in_shape(),
            "position {:?} is still outside {:?}",
            j.position,
            j.shape
        );
        // The repair is a hull of the stored points, so the original area is still inside
        // it and the ring is still a closed counter-clockwise ring on the grid.
        assert!(point_in_ring(&j.shape, Vec3::new_2d(5.5, 5.5)));
        for p in &shape {
            assert!(
                point_in_ring(&j.shape, *p) || ring_distance_sq_2d(&j.shape, *p) <= 1e-12,
                "the hull dropped the original vertex {p:?}"
            );
        }
        assert_eq!(j.shape[0], j.shape[j.shape.len() - 1], "closed");
        assert!(ring_signed_area_2x(&j.shape) > 0.0, "counter-clockwise");
        for p in &j.shape {
            assert!(is_on_grid(p.x, Q_POSITION_M) && is_on_grid(p.y, Q_POSITION_M));
        }

        // A junction whose position is already inside keeps its shape untouched.
        let inside = Junction {
            id: JunctionId::new(0),
            position: Vec3::new(5.5, 5.5, 0.0),
            shape: shape.clone(),
            incoming: Vec::new(),
            outgoing: Vec::new(),
            internal: Vec::new(),
            control: JunctionControl::Uncontrolled,
            conflicts: ConflictMatrix::new(0),
            name: None,
        };
        let mut net =
            RoadNetwork::new(Vec::new(), Vec::new(), vec![inside], Vec::new(), Vec::new()).unwrap();
        net.quantise_in_place();
        assert_eq!(net.junctions()[0].shape, shape, "no repair, no change");
    }

    /// R12: a transformation's float parameters are exported floats, so D9 applies to
    /// them. They are quantised when they are recorded, and the scan visits them.
    #[test]
    fn transformation_float_parameters_are_quantised_on_their_declared_grid() {
        let t = Transformation::new("grid")
            // The reviewer's case: a block size a fraction of a nanometre off the grid.
            .with("block_x_m", 120.000_000_123_456_7)
            .with("building_height_m", 20.000_000_9)
            // A degree is on the degree grid, not the metre grid — quantising it to
            // millimetres would move a world's origin by 50 m.
            .with("origin_lon_deg", -73.995_521_6)
            .with("min_lat", 40.744_000_12)
            // Seconds, speeds and decibels have their own quanta.
            .with("cycle_s", 90.000_000_4)
            .with("speed_limit_mps", 13.890_000_7)
            .with("gain_dbi", 5.004_9)
            // A quantum recorded as a parameter must survive being recorded.
            .with("degrees", Q_DEGREES)
            .with("position_m", Q_POSITION_M)
            // Integers, booleans and strings are not floats and are untouched.
            .with("cols", 6u32)
            .with("signalised", true)
            .with("algorithm", "rdp");

        assert_eq!(t.params["block_x_m"], serde_json::json!(120.0));
        assert_eq!(t.params["building_height_m"], serde_json::json!(20.0));
        assert_eq!(t.params["origin_lon_deg"], serde_json::json!(-73.995_521_6));
        assert_eq!(t.params["min_lat"], serde_json::json!(40.744_000_1));
        assert_eq!(t.params["cycle_s"], serde_json::json!(90.0));
        assert_eq!(t.params["speed_limit_mps"], serde_json::json!(13.89));
        assert_eq!(t.params["gain_dbi"], serde_json::json!(5.0));
        assert_eq!(t.params["degrees"], serde_json::json!(Q_DEGREES));
        assert_eq!(t.params["position_m"], serde_json::json!(Q_POSITION_M));
        assert_eq!(t.params["cols"], serde_json::json!(6));
        assert_eq!(t.params["signalised"], serde_json::json!(true));
        assert_eq!(t.params["algorithm"], serde_json::json!("rdp"));

        // Every float is on its grid, and only the floats are visited.
        let visited: Vec<(String, f64, f64)> = t
            .float_params()
            .map(|(k, v, q)| (k.clone(), v, q))
            .collect();
        assert_eq!(visited.len(), 9, "{visited:#?}");
        for (key, value, quantum) in &visited {
            assert!(
                is_on_grid(*value, *quantum),
                "{key} = {value} is off the {quantum} grid"
            );
        }
        assert_eq!(Transformation::param_quantum("block_x_m"), Q_POSITION_M);
        assert_eq!(Transformation::param_quantum("cycle_s"), Q_TIME_S);
        assert_eq!(
            Transformation::param_quantum("speed_limit_mps"),
            Q_SPEED_MPS
        );
        assert_eq!(Transformation::param_quantum("gain_dbi"), Q_DB);
        assert_eq!(Transformation::param_quantum("min_lon"), Q_DEGREES);
        assert_eq!(
            Transformation::param_quantum("metres_per_level"),
            Q_DEGREES,
            "an unnamed unit falls back to the finest grid, which never coarsens"
        );
    }
}
