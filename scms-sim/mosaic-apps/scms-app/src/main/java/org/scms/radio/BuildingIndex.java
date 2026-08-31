/*
 * SPDX-License-Identifier: Apache-2.0
 * Building footprints as a LOS / NLOSb blockage oracle.
 *
 * InTAS ships 5.7 MB of REAL Ingolstadt building footprints next to the road network
 * (sumo/buildings.poly.xml, 21 755 <poly type="building"> rings) which nothing in this project
 * used before. This class parses that file ONCE per JVM, indexes the rings on a uniform grid, and
 * answers the only question the channel model asks of them:
 *
 *     is the straight line from the TRUE transmitter position to the receiver blocked by a building?
 *
 * which is the LOS vs NLOSb decision of the 3GPP TR 37.885 urban model (see {@link PathLoss}).
 *
 * <h2>Coordinate frame — the trap</h2>
 * SUMO writes polygon shapes in NET coordinates, i.e. UTM plus the net's own {@code netOffset}
 * (ingolstadt.net.xml: {@code netOffset="-464198.88,-4952821.58"}, {@code convBoundary} =
 * 209535.70,446612.64 .. 223119.88,457704.79). MOSAIC's cartesian frame is
 * {@code cartesian = UTM + cartesianOffset}, and the scenario generator sets
 * {@code cartesianOffset} to exactly that same {@code netOffset}
 * (scms-sim/scenarios/mapgen.py:1159-1176, net_projection). The two frames are therefore
 * IDENTICAL for a generated scenario and no re-projection is needed — but a mismatch would
 * silently corrupt every LOS/NLOS classification while still producing plausible-looking output,
 * so {@link #probe} keeps a running bounding box of the receiver positions actually queried and
 * warns once if they do not land inside the footprint bounding box. {@code SCMS_BUILDING_OFFSET}
 * ("dx,dy", metres) exists to correct a scenario whose offsets really do differ.
 *
 * <h2>Index</h2>
 * A uniform grid (cell = {@code SCMS_BUILDING_CELL_M}, default 50 m) in CSR form, mirroring the
 * broadcast spatial hash the Python engine uses in its reception loop — no geometry library, no
 * new dependency. A query walks only the cells the link segment actually crosses (Amanatides-Woo
 * DDA) and edge-tests the rings stamped into them, early-exiting on the first blocking ring.
 *
 * <h2>Known limitation</h2>
 * The test is edge-intersection only: a segment lying entirely INSIDE one ring (both endpoints
 * indoors) reports LOS. Vehicles and surveyed RSUs are on the road network, so this cannot occur
 * for the links the channel model asks about.
 */
package org.scms.radio;

import java.io.File;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

public final class BuildingIndex {

    /** Grid cell edge in metres; a link query walks only the cells its segment crosses. */
    private static final double DEFAULT_CELL_M = 50.0;
    /** Warn if a probed receiver position falls further than this outside the footprint bbox. */
    private static final double ALIGN_SLACK_M = 1000.0;

    // ---- geometry (flat arrays: 21 755 rings / ~130 k vertices is ~3 MB, no per-ring objects) ----
    private final double[] vx;
    private final double[] vy;
    private final int[] ringStart;      // length n+1, CSR over vx/vy
    private final double[] ringBox;     // 4n: minX, minY, maxX, maxY per ring
    private final int nRings;

    // ---- uniform grid (CSR: cellStart[c]..cellStart[c+1] indexes cellRing) ----
    private final double cell;
    private final int gx0;
    private final int gy0;
    private final int gw;
    private final int gh;
    private final int[] cellStart;
    private final int[] cellRing;

    private final double minX;
    private final double minY;
    private final double maxX;
    private final double maxY;
    private final String source;
    private final long parseMillis;

    // ---- query scratch (single-threaded MOSAIC application federate; guarded anyway) ----
    private final int[] stamp;
    private int queryId;
    private long queries;
    private long blockedQueries;

    // ---- projection-alignment audit ----
    private boolean probed;
    private double pMinX = Double.POSITIVE_INFINITY;
    private double pMinY = Double.POSITIVE_INFINITY;
    private double pMaxX = Double.NEGATIVE_INFINITY;
    private double pMaxY = Double.NEGATIVE_INFINITY;
    private boolean alignmentWarned;
    private boolean aligned = true;

    private BuildingIndex(double[] vx, double[] vy, int[] ringStart, double[] ringBox,
                          double cell, String source, long parseMillis) {
        this.vx = vx;
        this.vy = vy;
        this.ringStart = ringStart;
        this.ringBox = ringBox;
        this.nRings = ringStart.length - 1;
        this.cell = cell;
        this.source = source;
        this.parseMillis = parseMillis;
        this.stamp = new int[nRings];

        double mnx = Double.POSITIVE_INFINITY;
        double mny = Double.POSITIVE_INFINITY;
        double mxx = Double.NEGATIVE_INFINITY;
        double mxy = Double.NEGATIVE_INFINITY;
        for (int r = 0; r < nRings; r++) {
            mnx = Math.min(mnx, ringBox[4 * r]);
            mny = Math.min(mny, ringBox[4 * r + 1]);
            mxx = Math.max(mxx, ringBox[4 * r + 2]);
            mxy = Math.max(mxy, ringBox[4 * r + 3]);
        }
        this.minX = mnx;
        this.minY = mny;
        this.maxX = mxx;
        this.maxY = mxy;

        this.gx0 = (int) Math.floor(mnx / cell);
        this.gy0 = (int) Math.floor(mny / cell);
        this.gw = Math.max(1, (int) Math.floor(mxx / cell) - gx0 + 1);
        this.gh = Math.max(1, (int) Math.floor(mxy / cell) - gy0 + 1);

        // CSR build: count per cell, prefix-sum, fill. Each ring is stamped into every cell its
        // bounding box touches (a building is ~10 m across, so typically 1-4 cells at 50 m).
        int nCells = gw * gh;
        int[] counts = new int[nCells];
        for (int r = 0; r < nRings; r++) {
            int cx0 = clampX((int) Math.floor(ringBox[4 * r] / cell));
            int cy0 = clampY((int) Math.floor(ringBox[4 * r + 1] / cell));
            int cx1 = clampX((int) Math.floor(ringBox[4 * r + 2] / cell));
            int cy1 = clampY((int) Math.floor(ringBox[4 * r + 3] / cell));
            for (int cy = cy0; cy <= cy1; cy++) {
                for (int cx = cx0; cx <= cx1; cx++) {
                    counts[(cy - gy0) * gw + (cx - gx0)]++;
                }
            }
        }
        int[] start = new int[nCells + 1];
        int acc = 0;
        for (int c = 0; c < nCells; c++) {
            start[c] = acc;
            acc += counts[c];
        }
        start[nCells] = acc;
        int[] ring = new int[acc];
        int[] cursor = java.util.Arrays.copyOf(start, nCells);
        for (int r = 0; r < nRings; r++) {
            int cx0 = clampX((int) Math.floor(ringBox[4 * r] / cell));
            int cy0 = clampY((int) Math.floor(ringBox[4 * r + 1] / cell));
            int cx1 = clampX((int) Math.floor(ringBox[4 * r + 2] / cell));
            int cy1 = clampY((int) Math.floor(ringBox[4 * r + 3] / cell));
            for (int cy = cy0; cy <= cy1; cy++) {
                for (int cx = cx0; cx <= cx1; cx++) {
                    ring[cursor[(cy - gy0) * gw + (cx - gx0)]++] = r;
                }
            }
        }
        this.cellStart = start;
        this.cellRing = ring;
    }

    private int clampX(int cx) {
        return Math.max(gx0, Math.min(gx0 + gw - 1, cx));
    }

    private int clampY(int cy) {
        return Math.max(gy0, Math.min(gy0 + gh - 1, cy));
    }

    // ------------------------------------------------------------------ loading

    /**
     * Locate and parse the scenario's building footprints.
     *
     * @param scenarioDir any directory inside the MOSAIC scenario (the application dir MOSAIC hands
     *                    the app is fine — the search walks up to three levels looking for
     *                    {@code sumo/buildings.poly.xml})
     * @return an index, or {@code null} when the scenario ships no footprints (synthetic grids)
     */
    public static BuildingIndex load(File scenarioDir, double cellM, double offX, double offY) {
        Path f = locate(scenarioDir);
        if (f == null) {
            return null;
        }
        long t0 = System.nanoTime();
        try {
            String xml = new String(Files.readAllBytes(f), StandardCharsets.UTF_8);
            Parsed p = parse(xml, offX, offY);
            if (p.ringStart.length <= 1) {
                return null;
            }
            long ms = (System.nanoTime() - t0) / 1_000_000L;
            return new BuildingIndex(p.vx, p.vy, p.ringStart, p.ringBox,
                    cellM > 0 ? cellM : DEFAULT_CELL_M,
                    f.toAbsolutePath().normalize().toString().replace('\\', '/'), ms);
        } catch (IOException | RuntimeException ex) {
            System.err.println("[BuildingIndex] cannot read " + f + ": " + ex);
            return null;
        }
    }

    /** {@code SCMS_BUILDINGS_FILE}, else {@code <scenario>/sumo/buildings.poly.xml} up to 3 levels up. */
    private static Path locate(File scenarioDir) {
        String explicit = System.getenv("SCMS_BUILDINGS_FILE");
        if (explicit != null && !explicit.isBlank()) {
            Path p = Path.of(explicit.trim());
            return Files.isRegularFile(p) ? p : null;
        }
        if (scenarioDir == null) {
            return null;
        }
        Path base = scenarioDir.toPath().toAbsolutePath().normalize();
        for (int up = 0; up < 3 && base != null; up++) {
            Path a = base.resolve("sumo").resolve("buildings.poly.xml");
            if (Files.isRegularFile(a)) {
                return a;
            }
            Path b = base.resolve("buildings.poly.xml");
            if (Files.isRegularFile(b)) {
                return b;
            }
            base = base.getParent();
        }
        return null;
    }

    private static final class Parsed {
        double[] vx;
        double[] vy;
        int[] ringStart;
        double[] ringBox;
    }

    /**
     * Hand-rolled scan for {@code <poly ... type="building" ... shape="x,y x,y ..."/>}.
     *
     * <p>A DOM parse of a 5.7 MB additional-file would allocate an object per attribute and pull in
     * an XML entity resolver for a file that is a flat list of one element type; this walks the
     * character buffer once instead. Only {@code type="building"} is taken — the same file also
     * carries {@code amenity} (622) and {@code parking} (59) polygons, which are not walls.
     */
    private static Parsed parse(String xml, double offX, double offY) {
        List<double[]> ringsX = new ArrayList<>();
        List<double[]> ringsY = new ArrayList<>();
        int total = 0;
        int i = xml.indexOf("<poly");
        while (i >= 0) {
            int end = xml.indexOf("/>", i);
            if (end < 0) {
                break;
            }
            String el = xml.substring(i, end);
            if (el.contains("type=\"building\"")) {
                String shape = attr(el, "shape=\"");
                if (shape != null) {
                    double[][] ring = points(shape, offX, offY);
                    if (ring != null && ring[0].length >= 3) {
                        ringsX.add(ring[0]);
                        ringsY.add(ring[1]);
                        total += ring[0].length;
                    }
                }
            }
            i = xml.indexOf("<poly", end);
        }
        Parsed p = new Parsed();
        int n = ringsX.size();
        p.vx = new double[total];
        p.vy = new double[total];
        p.ringStart = new int[n + 1];
        p.ringBox = new double[4 * n];
        int at = 0;
        for (int r = 0; r < n; r++) {
            double[] rx = ringsX.get(r);
            double[] ry = ringsY.get(r);
            p.ringStart[r] = at;
            double mnx = Double.POSITIVE_INFINITY;
            double mny = Double.POSITIVE_INFINITY;
            double mxx = Double.NEGATIVE_INFINITY;
            double mxy = Double.NEGATIVE_INFINITY;
            for (int k = 0; k < rx.length; k++) {
                p.vx[at] = rx[k];
                p.vy[at] = ry[k];
                at++;
                mnx = Math.min(mnx, rx[k]);
                mny = Math.min(mny, ry[k]);
                mxx = Math.max(mxx, rx[k]);
                mxy = Math.max(mxy, ry[k]);
            }
            p.ringBox[4 * r] = mnx;
            p.ringBox[4 * r + 1] = mny;
            p.ringBox[4 * r + 2] = mxx;
            p.ringBox[4 * r + 3] = mxy;
        }
        p.ringStart[n] = at;
        return p;
    }

    private static String attr(String el, String key) {
        int a = el.indexOf(key);
        if (a < 0) {
            return null;
        }
        a += key.length();
        int b = el.indexOf('"', a);
        return b < 0 ? null : el.substring(a, b);
    }

    /** {@code "x,y x,y ..."} -> two parallel arrays, with the closing duplicate vertex dropped. */
    private static double[][] points(String shape, double offX, double offY) {
        String[] parts = shape.trim().split("\\s+");
        double[] xs = new double[parts.length];
        double[] ys = new double[parts.length];
        int n = 0;
        for (String part : parts) {
            int c = part.indexOf(',');
            if (c <= 0) {
                continue;
            }
            try {
                double x = Double.parseDouble(part.substring(0, c)) + offX;
                // SUMO polygon shapes may carry a z coordinate ("x,y,z"); everything after the
                // second comma is elevation and is ignored (the blockage test is 2-D).
                String rest = part.substring(c + 1);
                int c2 = rest.indexOf(',');
                double y = Double.parseDouble(c2 < 0 ? rest : rest.substring(0, c2)) + offY;
                xs[n] = x;
                ys[n] = y;
                n++;
            } catch (NumberFormatException ex) {
                return null;
            }
        }
        // SUMO writes closed rings (last vertex == first); drop the duplicate and close implicitly.
        if (n >= 2 && xs[0] == xs[n - 1] && ys[0] == ys[n - 1]) {
            n--;
        }
        if (n < 3) {
            return null;
        }
        return new double[][] {java.util.Arrays.copyOf(xs, n), java.util.Arrays.copyOf(ys, n)};
    }

    // ------------------------------------------------------------------ query

    /**
     * True if a building footprint intersects the segment (x1,y1)-(x2,y2), i.e. the link is NLOSb.
     *
     * <p>Synchronized: the ring-dedup stamp array is shared scratch. MOSAIC's application federate
     * is single-threaded, so this is uncontended, and correctness does not depend on that.
     */
    public synchronized boolean blocked(double x1, double y1, double x2, double y2) {
        queries++;
        if (nRings == 0) {
            return false;
        }
        // cheap reject: the whole segment outside the footprint bounding box cannot be blocked
        if ((Math.max(x1, x2) < minX) || (Math.min(x1, x2) > maxX)
                || (Math.max(y1, y2) < minY) || (Math.min(y1, y2) > maxY)) {
            return false;
        }
        queryId++;
        boolean hit = walk(x1, y1, x2, y2);
        if (hit) {
            blockedQueries++;
        }
        return hit;
    }

    /**
     * Amanatides-Woo grid traversal over exactly the cells the segment crosses.
     *
     * <p>The segment is first Liang-Barsky clipped to the grid's own bounding box, so a link that
     * starts or ends outside the mapped area still walks the right cells (clamping the start cell
     * instead would desynchronise the DDA's t parameters from the true ray origin). Ring edge tests
     * always use the ORIGINAL endpoints, so clipping never changes an intersection verdict.
     */
    private boolean walk(double x1, double y1, double x2, double y2) {
        double dx = x2 - x1;
        double dy = y2 - y1;
        double gxLo = gx0 * cell;
        double gyLo = gy0 * cell;
        double gxHi = (gx0 + gw) * cell;
        double gyHi = (gy0 + gh) * cell;
        double t0 = 0.0;
        double t1 = 1.0;
        double[] p = {-dx, dx, -dy, dy};
        double[] q = {x1 - gxLo, gxHi - x1, y1 - gyLo, gyHi - y1};
        for (int k = 0; k < 4; k++) {
            if (p[k] == 0.0) {
                if (q[k] < 0.0) {
                    return false;                       // parallel and outside the slab
                }
            } else {
                double r = q[k] / p[k];
                if (p[k] < 0.0) {
                    t0 = Math.max(t0, r);
                } else {
                    t1 = Math.min(t1, r);
                }
            }
        }
        if (t0 > t1) {
            return false;                               // segment misses the grid entirely
        }
        double sx = x1 + t0 * dx;
        double sy = y1 + t0 * dy;
        double ex = x1 + t1 * dx;
        double ey = y1 + t1 * dy;
        double cdx = ex - sx;
        double cdy = ey - sy;
        int cx = clampX((int) Math.floor(sx / cell));
        int cy = clampY((int) Math.floor(sy / cell));
        final int cxEnd = clampX((int) Math.floor(ex / cell));
        final int cyEnd = clampY((int) Math.floor(ey / cell));
        int stepX = cdx > 0 ? 1 : (cdx < 0 ? -1 : 0);
        int stepY = cdy > 0 ? 1 : (cdy < 0 ? -1 : 0);
        double tMaxX = (stepX == 0) ? Double.POSITIVE_INFINITY
                : ((cx + (stepX > 0 ? 1 : 0)) * cell - sx) / cdx;
        double tMaxY = (stepY == 0) ? Double.POSITIVE_INFINITY
                : ((cy + (stepY > 0 ? 1 : 0)) * cell - sy) / cdy;
        double tDeltaX = (stepX == 0) ? Double.POSITIVE_INFINITY : Math.abs(cell / cdx);
        double tDeltaY = (stepY == 0) ? Double.POSITIVE_INFINITY : Math.abs(cell / cdy);
        // bounded: the DDA can never need more than the Manhattan cell span plus a slack step
        int budget = Math.abs(cxEnd - cx) + Math.abs(cyEnd - cy) + 2;
        while (budget-- > 0) {
            if (testCell(cx, cy, x1, y1, x2, y2)) {
                return true;
            }
            if (cx == cxEnd && cy == cyEnd) {
                return false;
            }
            if (tMaxX < tMaxY) {
                cx += stepX;
                tMaxX += tDeltaX;
            } else {
                cy += stepY;
                tMaxY += tDeltaY;
            }
            if (cx < gx0 || cx >= gx0 + gw || cy < gy0 || cy >= gy0 + gh) {
                return false;
            }
        }
        return false;
    }

    private boolean testCell(int cx, int cy, double x1, double y1, double x2, double y2) {
        int c = (cy - gy0) * gw + (cx - gx0);
        if (c < 0 || c >= gw * gh) {
            return false;
        }
        double sMinX = Math.min(x1, x2);
        double sMaxX = Math.max(x1, x2);
        double sMinY = Math.min(y1, y2);
        double sMaxY = Math.max(y1, y2);
        for (int k = cellStart[c]; k < cellStart[c + 1]; k++) {
            int r = cellRing[k];
            if (stamp[r] == queryId) {
                continue;                     // already tested on this query
            }
            stamp[r] = queryId;
            if (ringBox[4 * r] > sMaxX || ringBox[4 * r + 2] < sMinX
                    || ringBox[4 * r + 1] > sMaxY || ringBox[4 * r + 3] < sMinY) {
                continue;                     // bbox reject
            }
            if (ringHit(r, x1, y1, x2, y2)) {
                return true;
            }
        }
        return false;
    }

    private boolean ringHit(int r, double x1, double y1, double x2, double y2) {
        int s = ringStart[r];
        int e = ringStart[r + 1];
        int n = e - s;
        for (int k = 0; k < n; k++) {
            int a = s + k;
            int b = s + ((k + 1) % n);
            if (segments(x1, y1, x2, y2, vx[a], vy[a], vx[b], vy[b])) {
                return true;
            }
        }
        return false;
    }

    /** Proper + collinear-overlap segment intersection (orientation test, no library). */
    private static boolean segments(double ax, double ay, double bx, double by,
                                    double cx, double cy, double dx, double dy) {
        double d1 = cross(cx, cy, dx, dy, ax, ay);
        double d2 = cross(cx, cy, dx, dy, bx, by);
        double d3 = cross(ax, ay, bx, by, cx, cy);
        double d4 = cross(ax, ay, bx, by, dx, dy);
        if (((d1 > 0 && d2 < 0) || (d1 < 0 && d2 > 0))
                && ((d3 > 0 && d4 < 0) || (d3 < 0 && d4 > 0))) {
            return true;
        }
        return (d1 == 0 && onSeg(cx, cy, dx, dy, ax, ay))
                || (d2 == 0 && onSeg(cx, cy, dx, dy, bx, by))
                || (d3 == 0 && onSeg(ax, ay, bx, by, cx, cy))
                || (d4 == 0 && onSeg(ax, ay, bx, by, dx, dy));
    }

    private static double cross(double ax, double ay, double bx, double by, double px, double py) {
        return (bx - ax) * (py - ay) - (by - ay) * (px - ax);
    }

    private static boolean onSeg(double ax, double ay, double bx, double by, double px, double py) {
        return Math.min(ax, bx) <= px && px <= Math.max(ax, bx)
                && Math.min(ay, by) <= py && py <= Math.max(ay, by);
    }

    // ------------------------------------------------------- projection-alignment audit

    /**
     * Record a receiver position so a frame mismatch between the footprints and MOSAIC's cartesian
     * coordinates is LOUD instead of silent (see the class comment). Returns true while the probed
     * positions look aligned with the footprints.
     */
    public synchronized boolean probe(double x, double y) {
        pMinX = Math.min(pMinX, x);
        pMinY = Math.min(pMinY, y);
        pMaxX = Math.max(pMaxX, x);
        pMaxY = Math.max(pMaxY, y);
        if (!probed) {
            probed = true;
            boolean inside = x >= minX - ALIGN_SLACK_M && x <= maxX + ALIGN_SLACK_M
                    && y >= minY - ALIGN_SLACK_M && y <= maxY + ALIGN_SLACK_M;
            if (!inside) {
                aligned = false;
                if (!alignmentWarned) {
                    alignmentWarned = true;
                    System.err.println("[BuildingIndex] PROJECTION MISMATCH: receiver at ("
                            + Math.round(x) + ", " + Math.round(y) + ") is outside the footprint bbox ["
                            + Math.round(minX) + "," + Math.round(minY) + " .. "
                            + Math.round(maxX) + "," + Math.round(maxY)
                            + "] — every link will classify as LOS. Set SCMS_BUILDING_OFFSET=dx,dy.");
                }
            }
        }
        return aligned;
    }

    public boolean aligned() {
        return aligned;
    }

    public int ringCount() {
        return nRings;
    }

    public int vertexCount() {
        return ringStart[nRings];
    }

    public String source() {
        return source;
    }

    public long parseMillis() {
        return parseMillis;
    }

    public synchronized long queries() {
        return queries;
    }

    public synchronized long blockedQueries() {
        return blockedQueries;
    }

    /**
     * STATIC facts about the loaded index — what was parsed and how it was indexed. Safe to capture
     * at start-up, which is when {@code manifest.effective_params} is snapshotted; deliberately
     * carries no live counters, because a counter frozen at start-up reads as zero for the whole run
     * and quietly lies about whether the model did anything.
     */
    public Map<String, Object> describe() {
        Map<String, Object> m = new LinkedHashMap<>();
        m.put("source", source);
        m.put("buildings", nRings);
        m.put("vertices", vertexCount());
        m.put("cell_m", cell);
        m.put("grid", gw + "x" + gh);
        m.put("bbox", new double[] {round2(minX), round2(minY), round2(maxX), round2(maxY)});
        m.put("parse_ms", parseMillis);
        return m;
    }

    /** {@link #describe()} plus the LIVE query counters and the projection-alignment verdict.
     *  Read at shutdown, when those numbers mean something. */
    public synchronized Map<String, Object> stats() {
        Map<String, Object> m = describe();
        m.put("aligned", aligned);
        m.put("link_queries", queries);
        m.put("nlosb_queries", blockedQueries);
        m.put("nlosb_fraction", queries > 0 ? Math.round(1000.0 * blockedQueries / queries) / 1000.0 : 0.0);
        return m;
    }

    private static double round2(double v) {
        return Math.round(v * 100.0) / 100.0;
    }
}
