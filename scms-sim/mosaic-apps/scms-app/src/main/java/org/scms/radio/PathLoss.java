/*
 * SPDX-License-Identifier: Apache-2.0
 * 3GPP TR 37.885 V2X large-scale path loss + shadowing constants.
 *
 * ONE audited copy of the constants, shared by every Java-side channel decision. The values are the
 * same ones the Python engine grades against in
 * src/scms_sim_ref/datagen/refdata/pathloss_3gpp_tr37885.json (entries urban_los / urban_nlos /
 * highway_los / nlosv_extra_loss_db / shadowing_sigma_db / reference_frequency_ghz), so a divergence
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
 * NLOSv (blocked by a VEHICLE rather than a building) is included for completeness and is NOT
 * applied by the current Java receiver: MOSAIC's SNS gives no per-link vehicle occupancy and the
 * app has no fleet-wide rectangle index, so claiming an NLOSv classification would be fabricated.
 */
package org.scms.radio;

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

    /** Log-normal shadowing standard deviation (pathloss_3gpp_tr37885.shadowing_sigma_db). */
    public static final double SHADOW_SIGMA_LOS_DB = 3.0;
    public static final double SHADOW_SIGMA_NLOS_DB = 4.0;
    /** Gudmundson AR(1) decorrelation distance (PHASE2-DESIGN.md, model stack step 2/3). */
    public static final double DECORRELATION_LOS_M = 10.0;
    public static final double DECORRELATION_NLOS_M = 13.0;

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
     * Mean additional NLOSv loss in dB: {@code mu = muBase + max(0, 15*log10(d) - 41)} with
     * muBase 9.0 when both antennas are below the blocking vehicle and 5.0 when only one is
     * (sigma 4.5 / 4.0 dB respectively). Provided for parity with the Python engine's NLOSv term;
     * see the class comment for why the Java receiver does not apply it.
     */
    public static double nlosvMeanDb(double d, boolean bothBelowBlocker) {
        return (bothBelowBlocker ? 9.0 : 5.0) + Math.max(0.0, 15.0 * log10d(d) - 41.0);
    }

    public static double nlosvSigmaDb(boolean bothBelowBlocker) {
        return bothBelowBlocker ? 4.5 : 4.0;
    }

    public static double shadowSigmaDb(boolean los) {
        return los ? SHADOW_SIGMA_LOS_DB : SHADOW_SIGMA_NLOS_DB;
    }

    public static double decorrelationM(boolean los) {
        return los ? DECORRELATION_LOS_M : DECORRELATION_NLOS_M;
    }

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

    private static double log10d(double d) {
        return Math.log10(Math.max(d, MIN_DISTANCE_M));
    }
}
