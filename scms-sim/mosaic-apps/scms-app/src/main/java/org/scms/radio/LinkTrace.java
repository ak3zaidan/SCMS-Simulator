/*
 * SPDX-License-Identifier: Apache-2.0
 * Opt-in per-link emission trace for the geometric radio.
 *
 * <h2>Why this exists</h2>
 * {@code counts.channel} in the run manifest is an AGGREGATE: frames sensed, frames delivered, and
 * where the losses went. That is enough to state a scenario-wide delivery ratio and nothing else --
 * it cannot produce a PDR-versus-distance curve, a link-state composition per distance band, or a
 * channel-busy-ratio time series, which are precisely the instruments the Python engine's
 * {@code datagen.awareness} applies to the other radio. Without them the two engines cannot be
 * compared, only asserted about.
 *
 * <p>This class writes one CSV row per (frame, receiver) reception decision taken by
 * {@link RxChannel#deliver}, carrying the quantities that decision was actually made from:
 *
 * <pre>
 *   t_s,rx,dist_m,state,rssi_dbm,outcome,rx_x,rx_y,tx_x,tx_y
 * </pre>
 *
 * where {@code state} is the radio's own LOS/NLOSb verdict, {@code rssi_dbm} the value it compared
 * against the sensitivity, and {@code outcome} one of {@code ok} / {@code geom} (dropped by the
 * link budget) / {@code cong} (dropped by the CSMA contention model) / {@code wx} (weather).
 *
 * <p>The four endpoint coordinates are what make the Java LOS/NLOSb verdict AUDITABLE by a second
 * classifier: with them, the Python engine's {@code _BuildingRaster} can be run over exactly the
 * same segments, so a disagreement between the two engines' link-state composition can be split
 * into "different geometry test" and "different piece of city" instead of being asserted.
 *
 * <h2>Contract</h2>
 * <ul>
 *   <li>OFF unless {@code SCMS_LINK_TRACE} names a writable path, so the default run is unchanged.
 *   <li>Draws NO randomness from any stream the simulation uses. The per-frame sampling decision
 *       ({@code SCMS_LINK_TRACE_PROB}, default 1.0) is taken from a dedicated per-receiver
 *       {@code Random} seeded off the scenario seed, exactly like the shadowing streams, so
 *       enabling the trace cannot move a single delivery verdict.
 *   <li>ORACLE-derived (true distance, true LOS state). It is a diagnostic file next to the
 *       dataset, never a dataset row, and nothing in {@code ground_truth/} or {@code ma/} reads it.
 * </ul>
 */
package org.scms.radio;

import java.io.BufferedWriter;
import java.io.IOException;
import java.io.UncheckedIOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardOpenOption;
import java.util.Locale;

public final class LinkTrace {

    /** Destination CSV path; blank/unset disables the trace entirely. */
    private static final String PATH = trimmed(System.getenv("SCMS_LINK_TRACE"));
    /** Per-(frame, receiver) sampling probability, applied off a dedicated RNG stream. */
    public static final double PROB = prob();

    private static BufferedWriter out;
    private static boolean failed;
    private static long rows;

    private LinkTrace() {
    }

    public static boolean enabled() {
        return PATH != null && !failed;
    }

    public static String path() {
        return PATH;
    }

    public static long rows() {
        synchronized (LinkTrace.class) {
            return rows;
        }
    }

    /**
     * Append one reception decision. Cheap and synchronized: the MOSAIC application federate is
     * single-threaded, so this is uncontended, and a trace file must never be interleaved anyway.
     */
    public static synchronized void row(double t, String rx, double distM, String state,
                                        double rssiDbm, String outcome,
                                        double rxX, double rxY, double txX, double txY) {
        if (!enabled()) {
            return;
        }
        try {
            writer().append(fmt(t, 3)).append(',').append(rx).append(',')
                    .append(fmt(distM, 2)).append(',').append(state).append(',')
                    .append(fmt(rssiDbm, 2)).append(',').append(outcome).append(',')
                    .append(fmt(rxX, 2)).append(',').append(fmt(rxY, 2)).append(',')
                    .append(fmt(txX, 2)).append(',').append(fmt(txY, 2)).append('\n');
            rows++;
        } catch (IOException | UncheckedIOException ex) {
            failed = true;
            System.err.println("[LinkTrace] disabled after write failure: " + ex);
        }
    }

    private static BufferedWriter writer() throws IOException {
        if (out == null) {
            Path p = Path.of(PATH).toAbsolutePath().normalize();
            if (p.getParent() != null) {
                Files.createDirectories(p.getParent());
            }
            out = Files.newBufferedWriter(p, StandardCharsets.UTF_8, StandardOpenOption.CREATE,
                    StandardOpenOption.TRUNCATE_EXISTING, StandardOpenOption.WRITE);
            out.write("t_s,rx,dist_m,state,rssi_dbm,outcome,rx_x,rx_y,tx_x,tx_y\n");
            Runtime.getRuntime().addShutdownHook(new Thread(LinkTrace::close, "link-trace-close"));
            System.out.println("[LinkTrace] writing per-link reception trace to " + p
                    + " (sample probability " + PROB + ")");
        }
        return out;
    }

    /** Flush and close; idempotent. Registered as a JVM shutdown hook on first write. */
    public static synchronized void close() {
        if (out == null) {
            return;
        }
        try {
            out.flush();
            out.close();
            System.out.println("[LinkTrace] " + rows + " reception rows written to " + PATH);
        } catch (IOException ex) {
            System.err.println("[LinkTrace] close failed: " + ex);
        } finally {
            out = null;
        }
    }

    private static String fmt(double v, int nd) {
        if (Double.isNaN(v)) {
            return "";
        }
        return String.format(Locale.ROOT, "%." + nd + "f", v);
    }

    private static String trimmed(String s) {
        return (s == null || s.isBlank()) ? null : s.trim();
    }

    private static double prob() {
        String e = trimmed(System.getenv("SCMS_LINK_TRACE_PROB"));
        try {
            double v = (e == null) ? 1.0 : Double.parseDouble(e);
            return Math.max(0.0, Math.min(1.0, v));
        } catch (NumberFormatException ex) {
            return 1.0;
        }
    }
}
