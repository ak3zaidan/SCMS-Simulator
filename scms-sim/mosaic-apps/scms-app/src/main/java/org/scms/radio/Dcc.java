/*
 * SPDX-License-Identifier: Apache-2.0
 * ETSI reactive Decentralized Congestion Control (TS 102 687) over a modelled channel busy ratio.
 *
 * The state table is the one pinned in
 * src/scms_sim_ref/datagen/refdata/etsi_cam_dcc.json -> entries.dcc_reactive_rate_table:
 *
 *     CBR  < 0.30            -> 10   Hz   (T_GenCam >= 0.10 s)
 *     0.30 <= CBR < 0.40     ->  5   Hz   (T_GenCam >= 0.20 s)
 *     0.40 <= CBR < 0.50     ->  2.5 Hz   (T_GenCam >= 0.40 s)
 *     0.50 <= CBR < 0.60     ->  2   Hz   (T_GenCam >= 0.50 s)
 *     CBR  >= 0.60           ->  1   Hz   (T_GenCam >= 1.00 s)
 *
 * The terminal 1 Hz state coincides exactly with ETSI EN 302 637-2's T_GenCamMax, so DCC never
 * stretches the CAM interval beyond the CAM standard's own heartbeat: it only RAISES the floor
 * from T_GenCamMin (0.1 s) towards T_GenCamMax as the channel fills.
 *
 * <h2>CBR without an event-driven PHY</h2>
 * MOSAIC's SNS has no MAC and reports no channel-busy time, so CBR is modelled the way the
 * closed-form 802.11p literature does it: busy fraction = frames sensed x frame airtime / window.
 * Airtime is the exact IEEE 802.11p 10 MHz OFDM frame duration
 *
 *     T = T_preamble(32 us) + T_signal(8 us) + ceil((16 + 8*L + 6) / N_DBPS) * 8 us
 *
 * which for the default 300-byte CAM at 6 Mb/s (N_DBPS = 48 bits/symbol) is
 * 40 us + 51*8 us = 448 us, reproducing refdata/phy_80211p_profile.json:frame_airtime_us
 * (300 B -> 2422 PSDU bits -> 51 OFDM symbols -> 448 us) exactly.
 *
 * <p>That reference set pins CBR under TWO definitions: PPDU airtime only, and PPDU plus 207.5 us
 * of MAC overhead (AIFS_BE 110 us + mean backoff 97.5 us). This class implements the PPDU-only
 * reading by default, because ETSI TS 102 687 defines CBR as the fraction of time the medium is
 * SENSED BUSY, and AIFS/backoff are precisely the intervals in which it is sensed idle.
 * {@code SCMS_DCC_MAC_OVERHEAD_US=207.5} selects the other reading. Sanity check against
 * phy_80211p_profile.cbr_from_load: 80 vehicles x 10 Hz x 448 us = 0.3584 (PPDU only) and
 * x 655.5 us = 0.5244 (with overhead), both exactly the pinned values.
 *
 * <h2>Security relevance</h2>
 * DCC is not cosmetic for a misbehaviour dataset: a rate drop from 10 Hz to 1 Hz thins the evidence
 * every honest witness can produce by an order of magnitude, while a flooding attacker that ignores
 * DCC keeps its own rate. That asymmetry is exactly the thing an MA has to survive, so the
 * suppression counters are recorded in the run manifest.
 */
package org.scms.radio;

public final class Dcc {

    /** CBR breakpoints and the CAM rate (Hz) permitted at or above each one. */
    private static final double[] CBR_BREAK = {0.30, 0.40, 0.50, 0.60};
    private static final double[] RATE_HZ = {10.0, 5.0, 2.5, 2.0, 1.0};

    /** IEEE 802.11p, 10 MHz channel: OFDM symbol 8 us, preamble+SIGNAL 40 us. */
    public static final double SYMBOL_US = 8.0;
    public static final double PREAMBLE_US = 40.0;
    public static final int SERVICE_BITS = 16;
    public static final int TAIL_BITS = 6;

    private final double airtimeS;
    private final double probeS;
    private final int probes;
    private final double stateHoldS;

    private final int[] ring;
    private int ringAt;
    private int ringFill;
    private int probeCount;
    /** Time of the first frame ever sensed; every probe boundary is derived from it, not accumulated. */
    private double origin = Double.NEGATIVE_INFINITY;
    private long probeIndex;

    private double cbr;
    private double rateHz = RATE_HZ[0];
    private double lastStateT = Double.NEGATIVE_INFINITY;

    private long suppressed;
    private long allowed;
    private double cbrSum;
    private long cbrSamples;
    private double cbrMax;

    /**
     * @param frameBytes  CAM frame size on air (SCMS_DCC_FRAME_BYTES, default 300)
     * @param rateMbps    PHY data rate (SCMS_DCC_DATA_RATE_MBPS, default 6.0 -> QPSK 1/2, 10 MHz)
     * @param probeS      CBR probe interval (ETSI measures over 100 ms)
     * @param windowS     CBR averaging window (ETSI averages probes over ~1 s)
     * @param stateHoldS  minimum time between DCC state changes (ETSI NDL_minDccSampling ~1 s)
     * @param macOverheadUs per-frame MAC time added to the busy estimate (SCMS_DCC_MAC_OVERHEAD_US,
     *                    default 0 = PPDU only; 207.5 selects the PPDU + AIFS/backoff reading)
     */
    public Dcc(int frameBytes, double rateMbps, double probeS, double windowS, double stateHoldS,
               double macOverheadUs) {
        this.airtimeS = airtimeSeconds(frameBytes, rateMbps) + Math.max(0.0, macOverheadUs) * 1e-6;
        this.probeS = Math.max(1e-3, probeS);
        this.probes = Math.max(1, (int) Math.round(Math.max(this.probeS, windowS) / this.probeS));
        this.stateHoldS = Math.max(0.0, stateHoldS);
        this.ring = new int[this.probes];
    }

    /** IEEE 802.11p 10 MHz frame duration in seconds for an L-byte MPDU at {@code rateMbps}. */
    public static double airtimeSeconds(int frameBytes, double rateMbps) {
        double nDbps = Math.max(1.0, rateMbps * 1e6 * SYMBOL_US * 1e-6);   // bits per OFDM symbol
        int bits = SERVICE_BITS + 8 * Math.max(1, frameBytes) + TAIL_BITS;
        int symbols = (int) Math.ceil(bits / nDbps);
        return (PREAMBLE_US + symbols * SYMBOL_US) * 1e-6;
    }

    public double airtimeS() {
        return airtimeS;
    }

    /**
     * Record one frame SENSED on the channel. "Sensed" means before any of the receiver's own
     * decode-side drops: DCC reacts to channel occupancy, not to what this station managed to
     * decode, so this must be called at the very top of the reception path.
     */
    public void sense(double t) {
        roll(t);
        probeCount++;
    }

    /**
     * Advance the probe ring to time {@code t}, closing every probe interval that has elapsed.
     *
     * <p>Boundaries are computed as {@code floor((t - origin) / probeS)} rather than accumulated by
     * repeated addition. Accumulating drifts against the caller's own clock, and a boundary landing
     * on the wrong side of a frame then inserts a spurious EMPTY probe while the frames pile into
     * its neighbour — which biases the windowed average low even though no frame was lost.
     */
    private void roll(double t) {
        if (origin == Double.NEGATIVE_INFINITY) {
            origin = t;
            probeIndex = 0;
            return;
        }
        long idx = (long) Math.floor((t - origin) / probeS);
        long steps = idx - probeIndex;
        if (steps <= 0) {
            return;
        }
        // Close the elapsed probe intervals: the first carries the frames counted since the last
        // roll, any further elapsed intervals were silent. More than `probes` steps flushes the
        // whole ring, so writing `probes` entries is enough.
        int n = (int) Math.min(steps, probes);
        for (int k = 0; k < n; k++) {
            ring[ringAt] = (k == 0) ? probeCount : 0;
            probeCount = 0;
            ringAt = (ringAt + 1) % probes;
            ringFill = Math.min(probes, ringFill + 1);
        }
        probeIndex = idx;
    }

    /** Current modelled channel busy ratio in [0, 1] over the averaging window. */
    public double cbr(double t) {
        roll(t);
        // Closed probes plus the partial one currently open, over the matching elapsed time, so the
        // numerator and the denominator always cover the same interval.
        int frames = probeCount;
        for (int k = 0; k < ringFill; k++) {
            frames += ring[k];
        }
        double partialS = (origin == Double.NEGATIVE_INFINITY) ? 0.0
                : Math.max(0.0, Math.min(probeS, t - (origin + probeIndex * probeS)));
        double windowS = probeS * ringFill + partialS;
        cbr = (windowS <= 0.0) ? 0.0 : Math.min(1.0, frames * airtimeS / windowS);
        return cbr;
    }

    /** Reactive-DCC minimum CAM interval (s) for a channel busy ratio, per the ETSI step table. */
    public static double minIntervalS(double cbrValue) {
        return 1.0 / rateHzFor(cbrValue);
    }

    /** Reactive-DCC permitted CAM rate (Hz) for a channel busy ratio. */
    public static double rateHzFor(double cbrValue) {
        for (int k = 0; k < CBR_BREAK.length; k++) {
            if (cbrValue < CBR_BREAK[k]) {
                return RATE_HZ[k];
            }
        }
        return RATE_HZ[RATE_HZ.length - 1];
    }

    /**
     * The CAM interval floor this station must respect right now, re-latching the DCC state at most
     * every {@code stateHoldS} as ETSI's reactive state machine requires (NDL_minDccSampling). The
     * CBR statistics are accumulated on the same schedule, so the mean reported in the manifest is a
     * mean over DCC decisions rather than over however often the caller happened to ask.
     */
    public double currentMinIntervalS(double t) {
        double c = cbr(t);
        if (lastStateT == Double.NEGATIVE_INFINITY || t - lastStateT >= stateHoldS) {
            lastStateT = t;
            rateHz = rateHzFor(c);
            cbrSum += c;
            cbrSamples++;
            cbrMax = Math.max(cbrMax, c);
        }
        return 1.0 / rateHz;
    }

    public void noteAllowed() {
        allowed++;
    }

    public void noteSuppressed() {
        suppressed++;
    }

    public double currentCbr() {
        return cbr;
    }

    public double currentRateHz() {
        return rateHz;
    }

    public long suppressedCount() {
        return suppressed;
    }

    public long allowedCount() {
        return allowed;
    }

    public long cbrSamples() {
        return cbrSamples;
    }

    public double cbrSum() {
        return cbrSum;
    }

    public double cbrMax() {
        return cbrMax;
    }
}
