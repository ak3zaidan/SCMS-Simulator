//! The geodetic anchor of the world frame, and the one sanctioned projection into it.
//!
//! Build decision **D6** makes this "one convention everywhere":
//!
//! > a local tangent plane in metres, East-North-Up, origin at the world's bounding-box
//! > south-west corner, recorded in `GeoOrigin { lat, lon, alt }` with the projection named
//! > in `WorldProvenance`.
//!
//! Everything inside the engine is [`Vec3`] metres in that plane ([`crate::geom`]). Degrees
//! appear at exactly two seams: a world importer turning OSM or a DEM into the world, and an
//! encoder turning a position into a CAM or a BSM, both of which carry latitude and
//! longitude in tenths of microdegrees. [`GeoOrigin`] is the conversion at both seams, and
//! it is here — in the contract crate — so that the world importer, the message codec, the
//! UI's map overlay and any exporter that writes GeoJSON all apply *the same* projection to
//! the same origin. Two of them applying slightly different ones would put a vehicle in a
//! different building depending on which artefact you looked at.

use serde::{Deserialize, Serialize};

use crate::geom::Vec3;
use crate::math;

/// Degrees to radians, as an exact constant rather than `f64::to_radians`.
///
/// The value is identical — `to_radians` is this multiplication — but writing it out keeps
/// every step of a projection visibly inside the documented set of deterministic operations
/// (`+ - * /` and [`crate::math`]), which is what a reviewer checking ADR 0004 §4 has to be
/// able to see at a glance.
const DEG_TO_RAD: f64 = core::f64::consts::PI / 180.0;

/// The geodetic anchor of a world's local tangent plane (build decision D6).
///
/// World coordinate `(0, 0, 0)` is this point. A world records one of these and never
/// changes it, because every metre coordinate in the run — lane geometry, building
/// footprints, vehicle positions, recorded keyframes — is relative to it.
///
/// # The projection, exactly
///
/// An **equirectangular local tangent plane**: the scale factors are evaluated once, at the
/// origin's latitude, and applied linearly.
///
/// ```text
/// φ  = lat_deg_origin · π/180
/// m_per_deg_lat = 111132.92 − 559.82·cos 2φ + 1.175·cos 4φ
/// m_per_deg_lon = 111412.84·cos φ − 93.5·cos 3φ
///
/// east  = (lon − lon_origin) · m_per_deg_lon
/// north = (lat − lat_origin) · m_per_deg_lat
/// up    = alt − alt_origin
/// ```
///
/// The two series are the standard Meeus/WGS-84 metres-per-degree expansions, so the scale
/// is right for the ellipsoid at the origin's latitude rather than for a sphere. The
/// projection id, for `WorldProvenance`, is [`GeoOrigin::PROJECTION`].
///
/// # Its error, measured
///
/// The approximation is that the *longitude* scale is taken at the origin's latitude rather
/// than at each point's, so meridian convergence is ignored: a point north of the origin is
/// placed slightly too far east, growing linearly with the product of the two offsets. The
/// crate's own test measures the deviation from an exact WGS-84 ECEF→ENU conversion over a
/// **5 km × 5 km** extent and pins it:
///
/// | Origin latitude | Max error over ±2.5 km | Max error over a 5 km quadrant |
/// |---|---|---|
/// | 0° (equator) | 3.3 mm | 6.3 mm |
/// | 40.744° (the Phase 1 world, D7) | 0.95 m | 3.78 m |
/// | 60° | 1.90 m | 7.57 m |
///
/// At the Phase 1 latitude, a world whose origin is its bounding box's **south-west
/// corner** (as D6 requires) spans one quadrant, so 3.8 m over 5 km is the figure that
/// applies — about 0.08 % of the distance, and a *smooth* distortion: two points 50 m apart
/// are still 50 m apart to within a millimetre, because the error is almost entirely a
/// slow shear rather than local noise. Nothing the simulator computes is sensitive to it: a
/// path loss over 300 m changes by 0.006 dB, a headway does not change at all, and a
/// certificate does not care. What *would* be sensitive is comparing an engine position
/// against an external survey, and for that the projection is named in the provenance.
///
/// Determinism is unaffected: the error is a smooth deterministic function of position, and
/// every transcendental goes through [`crate::math`], so the value is bit-identical on every
/// platform.
///
/// # Duplication note
///
/// `v2xw-world` carries its own `GeoOrigin` and a cached `Projection` with the same fields,
/// the same series and the same projection id, and its `quant::Q_DEGREES` is the same
/// quantum as [`GeoOrigin::Q_DEG`] — declared there because this type had none. This is the
/// canonical definition; that one should become a re-export, and until it does the two must
/// not diverge.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct GeoOrigin {
    /// Latitude of world `(0, 0)`, degrees north, WGS-84.
    pub lat_deg: f64,
    /// Longitude of world `(0, 0)`, degrees east, WGS-84.
    pub lon_deg: f64,
    /// Ellipsoidal height of world `z = 0`, metres.
    pub alt_m: f64,
}

impl GeoOrigin {
    /// The projection's id, as recorded in the world's provenance (D6).
    ///
    /// Changing the formula means changing this string, never changing it silently: a
    /// recorded world says which projection produced its metres.
    pub const PROJECTION: &'static str = "equirectangular-local-tangent-plane/1";

    /// The origin of a world with no geodetic anchor: null island at sea level.
    ///
    /// A procedural or synthetic world has no real location, and `(0, 0, 0)` keeps that
    /// visible instead of inventing a city.
    pub const NULL_ISLAND: GeoOrigin = GeoOrigin {
        lat_deg: 0.0,
        lon_deg: 0.0,
        alt_m: 0.0,
    };

    /// The declared quantum for a geodetic latitude or longitude: `1e-7°`.
    ///
    /// Build decision D9's table has no entry for degrees, so it is declared here, once, in
    /// the type that owns the only degrees in the engine. `1e-7°` is 11 mm of latitude —
    /// the same order as the 1 mm metre grid the projected coordinates sit on, so the two
    /// grids are consistent — and it is also the resolution of the 1/10-microdegree
    /// latitude and longitude a CAM or a BSM carries, so an origin, a recorded world and a
    /// transmitted message all round to the same grid.
    pub const Q_DEG: f64 = 1e-7;

    /// The declared quantum for the origin's altitude: 1 mm, the metre grid of D9.
    pub const Q_ALT_M: f64 = 1e-3;

    /// Creates an origin from WGS-84 degrees and an ellipsoidal height.
    pub const fn new(lat_deg: f64, lon_deg: f64, alt_m: f64) -> Self {
        Self {
            lat_deg,
            lon_deg,
            alt_m,
        }
    }

    /// This origin with every float on its declared grid (build decision D9).
    ///
    /// The origin reaches a recorded artefact by four routes — `World::origin`, the world
    /// provenance, the run manifest and the UI's `Hello` — so it quantises like every other
    /// type that does. Worth doing once at construction as well as at the writer: the
    /// origin is the anchor every metre in the run is measured from, so an origin that
    /// moved by a rounding error between the import and the recording would move the whole
    /// world with it.
    ///
    /// For a digest, hash [`crate::math::grid_index`] of each field rather than these
    /// floats: the integer multiple is the value that is identical across platforms.
    pub fn quantized(&self) -> Self {
        Self {
            lat_deg: math::quantize_to(self.lat_deg, Self::Q_DEG),
            lon_deg: math::quantize_to(self.lon_deg, Self::Q_DEG),
            alt_m: math::quantize_to(self.alt_m, Self::Q_ALT_M),
        }
    }

    /// Metres per degree of latitude at this origin (the Meeus expansion above).
    ///
    /// An importer that projects hundreds of thousands of points should hoist this and
    /// [`GeoOrigin::metres_per_degree_longitude`] out of its loop: they are constant for the
    /// world, and each costs two [`crate::math::cos`] calls.
    pub fn metres_per_degree_latitude(&self) -> f64 {
        let phi = self.lat_deg * DEG_TO_RAD;
        111_132.92 - 559.82 * math::cos(2.0 * phi) + 1.175 * math::cos(4.0 * phi)
    }

    /// Metres per degree of longitude at this origin.
    ///
    /// Goes to zero at the poles, where the projection is meaningless and
    /// [`GeoOrigin::to_geodetic`] would divide by something arbitrarily small. A world at a
    /// pole is not a case this engine has.
    pub fn metres_per_degree_longitude(&self) -> f64 {
        let phi = self.lat_deg * DEG_TO_RAD;
        111_412.84 * math::cos(phi) - 93.5 * math::cos(3.0 * phi)
    }

    /// Projects WGS-84 degrees and an ellipsoidal height into world-local ENU metres.
    pub fn to_enu(&self, lat_deg: f64, lon_deg: f64, alt_m: f64) -> Vec3 {
        Vec3::new(
            (lon_deg - self.lon_deg) * self.metres_per_degree_longitude(),
            (lat_deg - self.lat_deg) * self.metres_per_degree_latitude(),
            alt_m - self.alt_m,
        )
    }

    /// The inverse of [`GeoOrigin::to_enu`]: world-local metres back to
    /// `(latitude_deg, longitude_deg, altitude_m)`.
    ///
    /// Exact in the sense that matters — the same approximation applied in reverse — so a
    /// round trip reproduces its input to within floating-point rounding and the
    /// projection's own error does not accumulate over one.
    pub fn to_geodetic(&self, p: Vec3) -> (f64, f64, f64) {
        (
            self.lat_deg + p.y / self.metres_per_degree_latitude(),
            self.lon_deg + p.x / self.metres_per_degree_longitude(),
            self.alt_m + p.z,
        )
    }

    /// True if the origin is a usable anchor: finite, latitude in `[-90, 90]`, longitude in
    /// `[-180, 180]`.
    pub fn is_valid(&self) -> bool {
        self.lat_deg.is_finite()
            && self.lon_deg.is_finite()
            && self.alt_m.is_finite()
            && (-90.0..=90.0).contains(&self.lat_deg)
            && (-180.0..=180.0).contains(&self.lon_deg)
    }
}

impl core::fmt::Display for GeoOrigin {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "({:.7}, {:.7}, {:.3} m)",
            self.lat_deg, self.lon_deg, self.alt_m
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Phase 1 world's south-west corner (build decision D7).
    const MANHATTAN: GeoOrigin = GeoOrigin::new(40.7440, -73.9900, 0.0);

    /// An exact WGS-84 geodetic → ECEF → ENU conversion, used **only** as the reference the
    /// projection's error is measured against.
    ///
    /// This is what the engine does *not* do: it needs a square root and three
    /// transcendentals per point and buys an accuracy nothing in the simulator can use.
    fn exact_enu(origin: GeoOrigin, lat_deg: f64, lon_deg: f64, alt_m: f64) -> Vec3 {
        const A: f64 = 6_378_137.0; // WGS-84 semi-major axis, m
        const E2: f64 = 6.694_379_990_141_32e-3; // first eccentricity squared

        let ecef = |lat_deg: f64, lon_deg: f64, h: f64| -> (f64, f64, f64) {
            let (phi, lam) = (lat_deg * DEG_TO_RAD, lon_deg * DEG_TO_RAD);
            let (sp, cp) = math::sin_cos(phi);
            let (sl, cl) = math::sin_cos(lam);
            let n = A / math::sqrt(1.0 - E2 * sp * sp);
            (
                (n + h) * cp * cl,
                (n + h) * cp * sl,
                (n * (1.0 - E2) + h) * sp,
            )
        };

        let (x0, y0, z0) = ecef(origin.lat_deg, origin.lon_deg, origin.alt_m);
        let (x, y, z) = ecef(lat_deg, lon_deg, alt_m);
        let (dx, dy, dz) = (x - x0, y - y0, z - z0);
        let (sp, cp) = math::sin_cos(origin.lat_deg * DEG_TO_RAD);
        let (sl, cl) = math::sin_cos(origin.lon_deg * DEG_TO_RAD);
        Vec3::new(
            -sl * dx + cl * dy,
            -sp * cl * dx - sp * sl * dy + cp * dz,
            cp * cl * dx + cp * sl * dy + sp * dz,
        )
    }

    /// The largest horizontal deviation from the exact conversion over a square of side
    /// `2 · half_extent_m` centred on the origin, or over the north-east quadrant of a
    /// square of side `extent` when `quadrant` is set (which is the D6 layout: the origin is
    /// the bounding box's south-west corner).
    fn max_error_m(origin: GeoOrigin, extent_m: f64, quadrant: bool) -> f64 {
        let steps = 20;
        let (lo, hi) = if quadrant {
            (0.0, extent_m)
        } else {
            (-extent_m / 2.0, extent_m / 2.0)
        };
        let mut worst: f64 = 0.0;
        for i in 0..=steps {
            for j in 0..=steps {
                let east = lo + (hi - lo) * (i as f64) / (steps as f64);
                let north = lo + (hi - lo) * (j as f64) / (steps as f64);
                // Go out to (east, north) through the projection, then ask the exact
                // conversion where that latitude and longitude really are.
                let (lat, lon, alt) = origin.to_geodetic(Vec3::new(east, north, 0.0));
                let truth = exact_enu(origin, lat, lon, alt);
                worst = worst.max((truth - Vec3::new(east, north, 0.0)).norm_2d());
            }
        }
        worst
    }

    /// The projection's documented error budget, measured rather than asserted. If these
    /// bounds move, the table in [`GeoOrigin`]'s documentation is wrong.
    #[test]
    fn the_projection_error_over_five_kilometres_is_what_the_docs_claim() {
        let equator = GeoOrigin::new(0.0, 0.0, 0.0);
        assert!(max_error_m(equator, 5_000.0, false) < 5e-3);
        assert!(max_error_m(equator, 5_000.0, true) < 1e-2);

        let centred = max_error_m(MANHATTAN, 5_000.0, false);
        assert!(
            (0.90..1.00).contains(&centred),
            "centred 5 km extent at 40.744 N: {centred} m"
        );
        let quadrant = max_error_m(MANHATTAN, 5_000.0, true);
        assert!(
            (3.70..3.90).contains(&quadrant),
            "5 km quadrant at 40.744 N: {quadrant} m"
        );

        let high = max_error_m(GeoOrigin::new(60.0, 10.0, 0.0), 5_000.0, true);
        assert!(
            (7.4..7.8).contains(&high),
            "5 km quadrant at 60 N: {high} m"
        );

        // The distortion is a shear, not noise: a short baseline keeps its length, which is
        // what every model in the engine actually measures.
        let a = MANHATTAN.to_enu(40.7540, -73.9800, 0.0);
        let b = MANHATTAN.to_enu(40.75445, -73.98, 0.0);
        let exact_a = exact_enu(MANHATTAN, 40.7540, -73.9800, 0.0);
        let exact_b = exact_enu(MANHATTAN, 40.75445, -73.98, 0.0);
        assert!(
            (a.distance_2d(b) - exact_a.distance_2d(exact_b)).abs() < 1e-3,
            "a 50 m baseline must keep its length to within a millimetre"
        );
    }

    /// Axes, signs and the scale, at a latitude where the two differ visibly.
    #[test]
    fn the_axes_point_where_the_convention_says() {
        let o = MANHATTAN;
        assert_eq!(o.to_enu(o.lat_deg, o.lon_deg, o.alt_m), Vec3::ZERO);

        let north = o.to_enu(o.lat_deg + 0.01, o.lon_deg, 0.0);
        assert!(north.y > 0.0 && north.x == 0.0, "+latitude is +y (north)");
        let east = o.to_enu(o.lat_deg, o.lon_deg + 0.01, 0.0);
        assert!(east.x > 0.0 && east.y == 0.0, "+longitude is +x (east)");
        let up = o.to_enu(o.lat_deg, o.lon_deg, 12.5);
        assert_eq!(up.z, 12.5, "+altitude is +z (up)");

        // A degree of longitude is shorter than a degree of latitude away from the equator,
        // and the two are equal to within 0.3 % at the equator itself.
        assert!(o.metres_per_degree_longitude() < o.metres_per_degree_latitude());
        assert!((o.metres_per_degree_latitude() - 111_049.0).abs() < 1.0);
        assert!((o.metres_per_degree_longitude() - 84_460.0).abs() < 1.0);
        let equator = GeoOrigin::new(0.0, 0.0, 0.0);
        assert!(
            (equator.metres_per_degree_longitude() / equator.metres_per_degree_latitude() - 1.0)
                .abs()
                < 7e-3
        );
        // Southern and western offsets are negative.
        assert!(o.to_enu(o.lat_deg - 0.01, o.lon_deg - 0.01, -5.0).x < 0.0);
        assert!(o.to_enu(o.lat_deg - 0.01, o.lon_deg - 0.01, -5.0).y < 0.0);
        assert_eq!(o.to_enu(o.lat_deg, o.lon_deg, -5.0).z, -5.0);
    }

    /// The round trip is the property importers and codecs rely on: project a point in,
    /// convert it back, and get the same degrees.
    #[test]
    fn the_projection_round_trips() {
        for o in [
            MANHATTAN,
            GeoOrigin::NULL_ISLAND,
            GeoOrigin::new(-33.8688, 151.2093, 58.0),
            GeoOrigin::new(60.0, -10.0, -12.5),
        ] {
            for (dlat, dlon, alt) in [
                (0.0, 0.0, 0.0),
                (0.01, 0.01, 30.0),
                (-0.02, 0.03, -7.5),
                (0.045, -0.06, 120.0),
            ] {
                let (lat, lon) = (o.lat_deg + dlat, o.lon_deg + dlon);
                let p = o.to_enu(lat, lon, alt);
                let (lat2, lon2, alt2) = o.to_geodetic(p);
                assert!((lat2 - lat).abs() < 1e-12, "{o}: {lat2} vs {lat}");
                assert!((lon2 - lon).abs() < 1e-12, "{o}: {lon2} vs {lon}");
                assert_eq!(alt2, alt);
                // …and back again, which must be idempotent in metres too.
                assert!(o.to_enu(lat2, lon2, alt2).distance(p) < 1e-6);
            }
        }
    }

    /// The projection is a pure function of the origin and the point: no state, no
    /// platform-dependent transcendental, so the same inputs give the same bits.
    #[test]
    fn projection_is_deterministic_and_pure() {
        let p1 = MANHATTAN.to_enu(40.7600, -73.9700, 3.25);
        let p2 = MANHATTAN.to_enu(40.7600, -73.9700, 3.25);
        assert_eq!(p1.x.to_bits(), p2.x.to_bits());
        assert_eq!(p1.y.to_bits(), p2.y.to_bits());
        assert_eq!(p1.z.to_bits(), p2.z.to_bits());

        // The scale factors are the documented series, evaluated through `crate::math`.
        let phi = MANHATTAN.lat_deg * DEG_TO_RAD;
        assert_eq!(
            MANHATTAN.metres_per_degree_latitude(),
            111_132.92 - 559.82 * math::cos(2.0 * phi) + 1.175 * math::cos(4.0 * phi)
        );
        assert_eq!(
            MANHATTAN.metres_per_degree_longitude(),
            111_412.84 * math::cos(phi) - 93.5 * math::cos(3.0 * phi)
        );
        assert_eq!(40.744_f64 * DEG_TO_RAD, 40.744_f64.to_radians());
    }

    #[test]
    fn origins_validate_and_round_trip_through_json() {
        assert!(MANHATTAN.is_valid());
        assert!(GeoOrigin::NULL_ISLAND.is_valid());
        assert_eq!(GeoOrigin::default(), GeoOrigin::NULL_ISLAND);
        assert!(!GeoOrigin::new(91.0, 0.0, 0.0).is_valid());
        assert!(!GeoOrigin::new(0.0, 181.0, 0.0).is_valid());
        assert!(!GeoOrigin::new(f64::NAN, 0.0, 0.0).is_valid());
        assert!(!GeoOrigin::new(0.0, 0.0, f64::INFINITY).is_valid());

        let json = serde_json::to_string(&MANHATTAN).unwrap();
        assert_eq!(json, r#"{"lat_deg":40.744,"lon_deg":-73.99,"alt_m":0.0}"#);
        assert_eq!(MANHATTAN.quantized(), MANHATTAN);
        assert_eq!(serde_json::from_str::<GeoOrigin>(&json).unwrap(), MANHATTAN);
        assert_eq!(MANHATTAN.to_string(), "(40.7440000, -73.9900000, 0.000 m)");
        assert_eq!(
            GeoOrigin::PROJECTION,
            "equirectangular-local-tangent-plane/1"
        );
    }

    /// The origin reaches four artefacts (`World::origin`, the world provenance, the
    /// manifest, the UI `Hello`), so it obeys the rule every other such type obeys: it
    /// declares its quanta and it quantises (03-interfaces.md §1, build decision D9). It
    /// was the one core type on that path with neither, which is why `v2xw-world` had to
    /// declare `Q_DEGREES` itself.
    #[test]
    fn the_origin_quantises_on_its_declared_grids() {
        assert_eq!(GeoOrigin::Q_DEG, 1e-7);
        assert_eq!(GeoOrigin::Q_ALT_M, 1e-3);

        let raw = GeoOrigin::new(40.744_000_049_9, -73.990_000_06, 12.500_499_9);
        let q = raw.quantized();
        assert!(math::is_on_grid(q.lat_deg, GeoOrigin::Q_DEG));
        assert!(math::is_on_grid(q.lon_deg, GeoOrigin::Q_DEG));
        assert!(math::is_on_grid(q.alt_m, GeoOrigin::Q_ALT_M));
        assert_eq!(q.lat_deg, 40.744);
        assert_eq!(q.lon_deg, -73.990_000_1);
        assert_eq!(q.alt_m, 12.5);
        assert_eq!(q.quantized(), q, "idempotent, as every quantiser must be");

        // The raw value really was off the grid, so the assertions above are not vacuous.
        assert!(!math::is_on_grid(raw.lat_deg, GeoOrigin::Q_DEG));
        assert!(!math::is_on_grid(raw.alt_m, GeoOrigin::Q_ALT_M));

        // A digest hashes the integer multiple, not the rounded float: the two `f64`s
        // either side of a grid point must digest identically.
        assert_eq!(
            math::grid_index(40.744_000_000_000_01, GeoOrigin::Q_DEG),
            math::grid_index(40.743_999_999_999_99, GeoOrigin::Q_DEG)
        );
        assert_eq!(math::grid_index(q.lat_deg, GeoOrigin::Q_DEG), 407_440_000);
        assert_eq!(math::grid_index(q.alt_m, GeoOrigin::Q_ALT_M), 12_500);

        // 1e-7° is 11 mm of latitude: the grid is the same order as the metre grid the
        // projected coordinates live on, which is the reason for the value.
        let one_quantum_north =
            GeoOrigin::new(MANHATTAN.lat_deg + GeoOrigin::Q_DEG, MANHATTAN.lon_deg, 0.0);
        let d = MANHATTAN
            .to_enu(one_quantum_north.lat_deg, one_quantum_north.lon_deg, 0.0)
            .norm_2d();
        assert!(
            (0.010..0.012).contains(&d),
            "one quantum of latitude is {d} m"
        );
    }
}
