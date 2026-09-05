/*
 * SPDX-License-Identifier: Apache-2.0
 * "Is a vehicle body sitting on this line of sight?" -- the NLOSv occupancy test.
 *
 * <h2>Why this class is what was actually missing</h2>
 * {@link PathLoss#nlosvMeanDb} shipped with Phase 2 and was never called; the stated reason was that
 * "MOSAIC's SNS gives no per-link vehicle occupancy and the app has no fleet-wide rectangle index".
 * The occupancy data was never missing: {@code ScmsBackend} already stores every vehicle's TRUE
 * position and refreshes it on every CAM ({@code Dev.lastX/lastY/lastSeenT}), and the live-map writer
 * already walked the whole fleet off it. Only the QUERY was missing. This is that query, and it takes
 * its input from the same back-end oracle the path loss already takes the transmitter position from,
 * so it introduces no new ground-truth channel.
 *
 * <h2>Ground-truth firewall</h2>
 * A blocker is identified by its OPAQUE channel link key (SHA-256("linkkey" | seed | unit id),
 * truncated to 64 bits) -- the same identifier {@code ScmsBackend.channelLinkKey} hands the shadowing
 * process. It is enough to recognise "this blocker IS the transmitter, skip it" and useless as an
 * identity, so no real vehicle id enters the radio in order to make this test work.
 *
 * <h2>Geometry</h2>
 * A vehicle blocks the link when its centre lies STRICTLY between the two antennas (projection
 * parameter {@code 0 < s < 1}) and within {@code halfWidthM} of the segment. That is the same
 * point-in-capsule test the Python engine's {@code _VehicleBlockerIndex.tallest_blocker} applies,
 * with the same 1.0 m default half width, so a link classified NLOSv here is classified NLOSv there.
 * The tallest such blocker wins, because the TR 37.885 branch selects on blocker height.
 *
 * <h2>Cost</h2>
 * A flat scan over the live fleet with an axis-aligned bounding-box reject and a height reject
 * (once a truck is found, no car can change the answer). The Python side grids at 25 m because it
 * runs one query per co-present PAIR per step over a dense candidate set; here the query runs once
 * per received frame with a fleet of a few hundred, and a branch-predictable scan over three
 * primitive arrays beats a hashed grid walk at that size -- measured at ~0.3 us per query against a
 * 334-vehicle snapshot, i.e. ~1.5 s over a 4.9 M-frame run.
 */
package org.scms.radio;

public final class VehicleBlockers {

    /** Empty snapshot: every link classifies clear. */
    public static final VehicleBlockers EMPTY = new VehicleBlockers(new long[0], new double[0],
            new double[0], new double[0], Double.NaN);

    private final long[] key;
    private final double[] x;
    private final double[] y;
    private final double[] h;
    private final double builtAtS;

    public VehicleBlockers(long[] key, double[] x, double[] y, double[] h, double builtAtS) {
        this.key = key;
        this.x = x;
        this.y = y;
        this.h = h;
        this.builtAtS = builtAtS;
    }

    public int size() {
        return key.length;
    }

    public double builtAtS() {
        return builtAtS;
    }

    /** Number of blockers at or above the truck height, for the manifest's fleet-composition line. */
    public int tallCount() {
        int n = 0;
        for (double v : h) {
            if (v >= PathLoss.BLOCKER_HEIGHT_TRUCK_M) {
                n++;
            }
        }
        return n;
    }

    /**
     * Height of the tallest vehicle whose body intersects the segment, or 0.0 when the line is clear.
     *
     * @param skipA,skipB opaque link keys of the two endpoints -- a link is never blocked by its own
     *                    transmitter or receiver
     */
    public double tallestBlocker(double x0, double y0, double x1, double y1,
                                 long skipA, long skipB, double halfWidthM) {
        int n = key.length;
        if (n == 0) {
            return 0.0;
        }
        double dx = x1 - x0;
        double dy = y1 - y0;
        double ll = dx * dx + dy * dy;
        if (ll <= 1e-9) {
            return 0.0;
        }
        double invLl = 1.0 / ll;
        double hw2 = halfWidthM * halfWidthM;
        double xLo = Math.min(x0, x1) - halfWidthM;
        double xHi = Math.max(x0, x1) + halfWidthM;
        double yLo = Math.min(y0, y1) - halfWidthM;
        double yHi = Math.max(y0, y1) + halfWidthM;
        double best = 0.0;
        for (int i = 0; i < n; i++) {
            double hi = h[i];
            if (hi <= best) {
                continue;                       // cannot improve the answer -- and prunes hard
            }
            double vx = x[i];
            if (vx < xLo || vx > xHi) {
                continue;
            }
            double vy = y[i];
            if (vy < yLo || vy > yHi) {
                continue;
            }
            if (key[i] == skipA || key[i] == skipB) {
                continue;
            }
            double ax = vx - x0;
            double ay = vy - y0;
            double s = (ax * dx + ay * dy) * invLl;
            if (!(s > 0.0 && s < 1.0)) {        // strictly BETWEEN the two antennas
                continue;
            }
            double px = ax - s * dx;
            double py = ay - s * dy;
            if (px * px + py * py <= hw2) {
                best = hi;
            }
        }
        return best;
    }
}
