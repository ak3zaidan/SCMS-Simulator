/**
 * WGS-84 → world-local ENU metres, the projection §0 of the protocol assumes ("world-local ENU
 * metres (East, North, Up) about `Hello.origin_{lat,lon,alt}`").
 *
 * A local tangent plane is exactly right here: the areas we serve are ~2 km across, where the
 * flat-earth error is well under the 1 mm pose quantum.
 */

/** Metres per degree of latitude at latitude `phiDeg` (WGS-84 series expansion). */
export function metresPerDegreeLat(phiDeg: number): number {
  const phi = (phiDeg * Math.PI) / 180;
  return 111132.92 - 559.82 * Math.cos(2 * phi) + 1.175 * Math.cos(4 * phi) - 0.0023 * Math.cos(6 * phi);
}

/** Metres per degree of longitude at latitude `phiDeg`. */
export function metresPerDegreeLon(phiDeg: number): number {
  const phi = (phiDeg * Math.PI) / 180;
  return 111412.84 * Math.cos(phi) - 93.5 * Math.cos(3 * phi) + 0.118 * Math.cos(5 * phi);
}

/** A geodetic bounding box, in the `[min_lon, min_lat, max_lon, max_lat]` order §6.11 uses. */
export type GeoBbox = readonly [number, number, number, number];

/** A local tangent-plane projection about a geodetic origin. */
export interface EnuProjection {
  readonly originLatDeg: number;
  readonly originLonDeg: number;
  readonly originAltM: number;
  readonly mPerDegLat: number;
  readonly mPerDegLon: number;
  /** Project a WGS-84 position to ENU metres about the origin. */
  project(lonDeg: number, latDeg: number): { x: number; y: number };
  /** Inverse of {@link project}. */
  unproject(x: number, y: number): { lonDeg: number; latDeg: number };
}

/** Build a projection whose origin is the south-west corner of `bbox`. */
export function projectionForBbox(bbox: GeoBbox, originAltM = 10): EnuProjection {
  const [minLon, minLat, , maxLat] = bbox;
  const midLat = (minLat + maxLat) / 2;
  const mPerDegLat = metresPerDegreeLat(midLat);
  const mPerDegLon = metresPerDegreeLon(midLat);
  return {
    originLatDeg: minLat,
    originLonDeg: minLon,
    originAltM,
    mPerDegLat,
    mPerDegLon,
    project: (lonDeg, latDeg) => ({ x: (lonDeg - minLon) * mPerDegLon, y: (latDeg - minLat) * mPerDegLat }),
    unproject: (x, y) => ({ lonDeg: minLon + x / mPerDegLon, latDeg: minLat + y / mPerDegLat }),
  };
}
