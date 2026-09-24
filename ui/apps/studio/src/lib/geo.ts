/**
 * World-local metres to WGS-84 degrees, the way the engine does it.
 *
 * The same local-tangent-plane approximation as `v2xw_core::geo::GeoOrigin` (Meeus's series for the
 * length of a degree), so a position the HUD prints from the stream and a latitude a BSM carries are
 * converted by one rule and can be compared. Over a city-sized world the approximation's error is
 * centimetres (the core crate's tests measure it against an exact ECEF conversion).
 */

const DEG = Math.PI / 180;

/** Metres per degree of latitude at `latDeg`. */
export function metresPerDegreeLatitude(latDeg: number): number {
  const phi = latDeg * DEG;
  return 111_132.92 - 559.82 * Math.cos(2 * phi) + 1.175 * Math.cos(4 * phi);
}

/** Metres per degree of longitude at `latDeg`. */
export function metresPerDegreeLongitude(latDeg: number): number {
  const phi = latDeg * DEG;
  return 111_412.84 * Math.cos(phi) - 93.5 * Math.cos(3 * phi);
}

/** World-local ENU metres to `(lat, lon)` degrees about `origin`. */
export function toGeodetic(origin: { lat: number; lon: number }, x: number, y: number): { lat: number; lon: number } {
  return {
    lat: origin.lat + y / metresPerDegreeLatitude(origin.lat),
    lon: origin.lon + x / metresPerDegreeLongitude(origin.lat),
  };
}

/** `(lat, lon)` degrees to world-local ENU metres about `origin`. */
export function toEnu(origin: { lat: number; lon: number }, lat: number, lon: number): { x: number; y: number } {
  return {
    x: (lon - origin.lon) * metresPerDegreeLongitude(origin.lat),
    y: (lat - origin.lat) * metresPerDegreeLatitude(origin.lat),
  };
}
