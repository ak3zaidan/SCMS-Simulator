/*
 * SPDX-License-Identifier: Apache-2.0
 * 3GPP TR 37.885 V2X large-scale path loss, NLOSv vehicle blockage, shadowing and small-scale
 * fading constants.
 *
 * ONE audited copy of the constants, shared by every Java-side channel decision. The values are the
 * same ones the Python engine grades against in
 * src/scms_sim_ref/datagen/refdata/pathloss_3gpp_tr37885.json (entries urban_los / urban_nlos /
 * highway_los / nlosv_extra_loss_db / nlosv_blocker_height_m / shadowing_sigma_db /
 * shadowing_decorrelation_distance_m / reference_frequency_ghz) and
 * refdata/nakagami_fading.json (m_by_distance_adopted, unit_mean_power_convention), so a divergence
 * between the two engines is a diff on this file, not a hunt through two reception loops.
 *
 *   PL_dB = a + b*log10(d_m) + c*log10(fc_GHz)
 *
 *   urban LOS    38.77 + 16.7*log10(d) + 18.2*log10(fc)   sigma 3.0 dB, decorrelation 10 m
 *   urban NLOSb  36.85 + 30.0*log10(d) + 18.9*log10(fc)   sigma 4.0 dB, decorrelation 13 m
 *   highway LOS  32.40 + 20.0*log10(d) + 20.0*log10(fc)   sigma 3.0 dB, decorrelation 10 m
 *
 * At the ITS-G5 carrier (fc = 5.9 GHz, log10 fc = 0.7708520) the frequency terms are fixed
 * offsets -- urban LOS 14.0295 dB, urban NLOS 14.5691 dB, highway LOS 15.4170 dB -- so the models
 * collapse to 52.7995 + 16.7*log10(d), 51.4191 + 30*log10(d) and 47.8170 + 20*log10(d).
 *
 * <h2>NLOSv (blocked by a VEHICLE), and why it used to be dead code</h2>
 * {@link #nlosvMeanDb} shipped here from the first Phase-2 commit and was never called. Two reasons,
 * one stale and one a real defect in THIS file:
 *
 * <ol>
 *   <li>The old class comment claimed "MOSAIC's SNS gives no per-link vehicle occupancy and the app
 *       has no fleet-wide rectangle index, so claiming an NLOSv classification would be fabricated".
 *       The first half was already false when it was written: {@code ScmsBackend} keeps every
 *       vehicle's TRUE position ({@code Dev.lastX/lastY/lastSeenT}, refreshed on every CAM) and
 *       {@code maybeWriteLive} already iterated the whole fleet off it. What was missing was not the
 *       occupancy data but a spatial query over it -- {@link VehicleBlockers}, added with this
 *       change, is that query and it reads the same oracle the path loss already reads.
 *   <li>The signature {@code nlosvMeanDb(double d, boolean bothBelowBlocker)} could not express the
 *       model. TR 37.885 has THREE branches -- both antennas below the blocker (mu base 9.0 dB,
 *       sigma 4.5), exactly one below (5.0 dB, sigma 4.0), and both ABOVE, which takes no extra loss
 *       at all and is not an NLOSv link. A boolean collapses "one below" and "both above" into one
 *       value, so a caller with an RSU antenna at 5 m over a 1.6 m car would have been charged 5 dB
 *       for a link the standard says is clear. The parameter is now the COUNT of antennas below the
 *       blocker (0/1/2) and the zero branch returns 0.0 dB, which is the shape the call site needs.
 * </ol>
 */
package org.scms.radio;

import java.util.Random;

public final class PathLoss {

    /** ITS-G5 / DSRC control channel (pathloss_3gpp_tr37885.reference_frequency_ghz). */
    public static final double FC_GHZ = 5.9;

    public static final double URBAN_LOS_A = 38.77;
    public static final double URBAN_LOS_B = 16.7;
    public static final double URBAN_LOS_C = 18.2;

    public static final double URBAN_NLOS_A = 36.85;
    public static final double URBAN_NLOS_B = 30.0;
    public static final double URBAN_NLOS_C = 18.9;

    public static final double HIGHWAY_LOS_A = 32.4;
    public static final double HIGHWAY_LOS_B = 20.0;
    public static final double HIGHWAY_LOS_C = 20.0;

    /**
     * The three link states the geometric receiver distinguishes. {@link #label()} is the token the
     * per-link trace writes and {@code tools/xengine_radio.py} bins on, and it matches the Python
     * engine's own vocabulary exactly (LOS / NLOSv / NLOSb).
     */
    public enum State {
        LOS("LOS", SHADOW_SIGMA_LOS_DB, DECORRELATION_LOS_M),
        /** Blocked by a VEHICLE: LOS path loss plus a censored-Gaussian blockage term. */
        NLOSV("NLOSv", SHADOW_SIGMA_LOS_DB, DECORRELATION_NLOS_M),
        /** Blocked by a BUILDING: its own path-loss formula, no extra term. */
        NLOSB("NLOSb", SHADOW_SIGMA_NLOS_DB, DECORRELATION_NLOS_M);

        private final String label;
        private final double sigmaDb;
        private final double decorrM;

        State(String label, double sigmaDb, double decorrM) {
            this.label = label;
            this.sigmaDb = sigmaDb;
            this.decorrM = decorrM;
        }

        public String label() {
            return label;
        }

        /**
         * Shadow-fading standard deviation (pathloss_3gpp_tr37885.shadowing_sigma_db). NLOSv keeps
         * the LOS 3.0 dB deliberately: its blockage spread is already a separate random variable
         * ({@link #nlosvSigmaDb}), and charging it the NLOSb 4.0 dB would double-count.
         */
        public double sigmaDb() {
            return sigmaDb;
        }

        /** Gudmundson AR(1) decorrelation distance (shadowing_decorrelation_distance_m). */
        public double decorrM() {
            return decorrM;
        }
    }

    /** Log-normal shadowing standard deviation (pathloss_3gpp_tr37885.shadowing_sigma_db). */
    public static final double SHADOW_SIGMA_LOS_DB = 3.0;
    public static final double SHADOW_SIGMA_NLOS_DB = 4.0;
    /** Gudmundson AR(1) decorrelation distance (PHASE2-DESIGN.md, model stack step 2/3). */
    public static final double DECORRELATION_LOS_M = 10.0;
    public static final double DECORRELATION_NLOS_M = 13.0;

    /**
     * NLOSv mean extra loss BASE, indexed by how many of the two antennas sit below the blocker.
     * {@code [0]} is the standard's "both above" branch: no extra loss, and therefore not an NLOSv
     * link at all. (pathloss_3gpp_tr37885.nlosv_extra_loss_db)
     */
    public static final double[] NLOSV_MU_BASE_DB = {0.0, 5.0, 9.0};
    /** Matching per-branch sigma; the zero branch is never drawn from. */
    public static final double[] NLOSV_SIGMA_DB = {0.0, 4.0, 4.5};
    /**
     * The distance term {@code max(0, 15*log10(d) - 41)} is identically zero below
     * {@code 10^(41/15) = 541.17 m} (pathloss_3gpp_tr37885.nlosv_distance_term_activation_m), i.e.
     * across the whole urban regime the mean extra loss is a constant 9.0 or 5.0 dB.
     */
    public static final double NLOSV_DISTANCE_TERM_ACTIVATION_M = 541.1695;

    /** Blocker heights (pathloss_3gpp_tr37885.nlosv_blocker_height_m). */
    public static final double BLOCKER_HEIGHT_CAR_M = 1.6;
    public static final double BLOCKER_HEIGHT_TRUCK_M = 3.0;
    /** Roof-mounted OBU antenna; below even the shortest blocker, hence the 9.0 dB branch. */
    public static final double ANTENNA_HEIGHT_VEHICLE_M = 1.5;
    /** Pole-mounted RSU antenna: above a car AND a truck blocker, hence the 5.0 dB branch. */
    public static final double ANTENNA_HEIGHT_RSU_M = 5.0;

    /**
     * Nakagami-m shape factor by link distance (nakagami_fading.m_by_distance_adopted): 3 / 1.5 /
     * 1.0 over 0-50 / 50-150 / >150 m. NOT the ns-3 default banding (1.5 / 0.75 / 0.75 at 80/200 m),
     * which that refdata entry pins separately and explicitly does not adopt.
     */
    public static final double NAKAGAMI_BAND_1_M = 50.0;
    public static final double NAKAGAMI_BAND_2_M = 150.0;
    public static final double NAKAGAMI_M_NEAR = 3.0;
    public static final double NAKAGAMI_M_MID = 1.5;
    public static final double NAKAGAMI_M_FAR = 1.0;

    /** Distance floor: the log-distance form diverges at d -> 0. */
    public static final double MIN_DISTANCE_M = 1.0;

    private PathLoss() {
    }

    /** Urban LOS path loss in dB (d in metres, fc in GHz). */
    public static double urbanLos(double d, double fcGhz) {
        return URBAN_LOS_A + URBAN_LOS_B * log10d(d) + URBAN_LOS_C * Math.log10(fcGhz);
    }

    /** Urban NLOS (building-blocked, NLOSb) path loss in dB. */
    public static double urbanNlos(double d, double fcGhz) {
        return URBAN_NLOS_A + URBAN_NLOS_B * log10d(d) + URBAN_NLOS_C * Math.log10(fcGhz);
    }

    /** Highway LOS path loss in dB (the free-space form). */
    public static double highwayLos(double d, double fcGhz) {
        return HIGHWAY_LOS_A + HIGHWAY_LOS_B * log10d(d) + HIGHWAY_LOS_C * Math.log10(fcGhz);
    }

    /**
     * How many of the two antennas sit below the blocking vehicle -- the TR 37.885 branch selector.
     * 0 means the standard applies no extra loss and the link stays LOS.
     */
    public static int antennasBelowBlocker(double txAntennaH, double rxAntennaH, double blockerH) {
        return (txAntennaH < blockerH ? 1 : 0) + (rxAntennaH < blockerH ? 1 : 0);
    }

    /**
     * Mean additional NLOSv loss in dB: {@code mu = muBase + max(0, 15*log10(d) - 41)} with muBase
     * 9.0 when BOTH antennas are below the blocking vehicle, 5.0 when exactly one is, and 0.0 (no
     * loss, no NLOSv state) when neither is.
     *
     * @param antennasBelowBlocker 0, 1 or 2 -- see {@link #antennasBelowBlocker}
     */
    public static double nlosvMeanDb(double d, int antennasBelowBlocker) {
        int b = clampBranch(antennasBelowBlocker);
        if (b == 0) {
            return 0.0;
        }
        return NLOSV_MU_BASE_DB[b] + Math.max(0.0, 15.0 * log10d(d) - 41.0);
    }

    /** Per-branch sigma of the NLOSv extra-loss Gaussian (4.5 dB both-below, 4.0 dB one-below). */
    public static double nlosvSigmaDb(int antennasBelowBlocker) {
        return NLOSV_SIGMA_DB[clampBranch(antennasBelowBlocker)];
    }

    /**
     * One draw of the NLOSv extra loss: {@code max(0, N(mu, sigma))}.
     *
     * <p>A CENSORED Gaussian, drawn and clamped -- never shortcut to the mean. Truncation at 0 makes
     * the realised mean strictly greater than mu whenever mu is within ~2 sigma of zero, which the
     * refdata entry calls out as the implementer trap.
     */
    public static double nlosvExtraLossDb(Random rng, double d, int antennasBelowBlocker) {
        int b = clampBranch(antennasBelowBlocker);
        if (b == 0) {
            return 0.0;
        }
        return Math.max(0.0, nlosvMeanDb(d, b) + NLOSV_SIGMA_DB[b] * rng.nextGaussian());
    }

    /** Nakagami shape factor for a link of this length (3 / 1.5 / 1.0 over 0-50 / 50-150 / >150 m). */
    public static double nakagamiM(double d) {
        if (d <= NAKAGAMI_BAND_1_M) {
            return NAKAGAMI_M_NEAR;
        }
        return d <= NAKAGAMI_BAND_2_M ? NAKAGAMI_M_MID : NAKAGAMI_M_FAR;
    }

    /**
     * One per-packet small-scale fading term in dB: {@code 10*log10(G)} with
     * {@code G ~ Gamma(shape = m, scale = 1/m)}.
     *
     * <p>Scale {@code 1/m} is what makes the gain UNIT MEAN (nakagami_fading
     * .unit_mean_power_convention): fading redistributes power without adding any. Using scale 1
     * would inflate mean received power by a factor of m -- +4.8 dB in the near band and 0 dB beyond
     * 150 m, a distance-dependent bias indistinguishable from a path-loss-exponent error.
     *
     * <p>Sampler: Marsaglia & Tsang (2000) squeeze method, valid for shape >= 1. All three adopted
     * bands (3.0 / 1.5 / 1.0) are >= 1, so the low-shape boost step is not needed and is deliberately
     * not implemented -- if a future banding adopts m &lt; 1 this method must gain it rather than
     * silently return a biased draw, hence the explicit guard.
     */
    public static double nakagamiFadeDb(Random rng, double m) {
        if (!(m >= 1.0)) {
            throw new IllegalArgumentException("Nakagami m < 1 needs the boost step: " + m);
        }
        double g = gammaUnitScale(rng, m) / m;
        return 10.0 * Math.log10(Math.max(g, 1e-12));
    }

    /** Gamma(shape = a, scale = 1) for a >= 1 -- Marsaglia & Tsang squeeze. */
    private static double gammaUnitScale(Random rng, double a) {
        double d = a - 1.0 / 3.0;
        double c = 1.0 / Math.sqrt(9.0 * d);
        while (true) {
            double x;
            double v;
            do {
                x = rng.nextGaussian();
                v = 1.0 + c * x;
            } while (v <= 0.0);
            v = v * v * v;
            double u = rng.nextDouble();
            double x2 = x * x;
            if (u < 1.0 - 0.0331 * x2 * x2) {
                return d * v;
            }
            if (Math.log(u) < 0.5 * x2 + d * (1.0 - v + Math.log(v))) {
                return d * v;
            }
        }
    }

    // The former shadowSigmaDb(boolean) / decorrelationM(boolean) helpers are GONE, folded into
    // State.sigmaDb() / State.decorrM(). A boolean cannot name three link states, and keeping a
    // two-state accessor alongside a three-state model is how PathLoss ended up with a method the
    // receiver could not correctly call in the first place.

    /**
     * Median link distance at which the mean received power crosses the receiver sensitivity, i.e.
     * the 50%-delivery range of this model before shadowing. Used only for the start-up log line, so
     * the operator can see immediately what range the configured link budget implies.
     */
    public static double medianRangeM(double budgetDb, boolean urban, boolean los, double fcGhz) {
        double a;
        double b;
        double c;
        if (!urban) {
            a = HIGHWAY_LOS_A;
            b = HIGHWAY_LOS_B;
            c = HIGHWAY_LOS_C;
        } else if (los) {
            a = URBAN_LOS_A;
            b = URBAN_LOS_B;
            c = URBAN_LOS_C;
        } else {
            a = URBAN_NLOS_A;
            b = URBAN_NLOS_B;
            c = URBAN_NLOS_C;
        }
        return Math.pow(10.0, (budgetDb - a - c * Math.log10(fcGhz)) / b);
    }

    private static int clampBranch(int n) {
        return n < 0 ? 0 : Math.min(n, 2);
    }

    private static double log10d(double d) {
        return Math.log10(Math.max(d, MIN_DISTANCE_M));
    }
}
