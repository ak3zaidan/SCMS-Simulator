/*
 * SPDX-License-Identifier: EPL-2.0
 *
 * Ported from VeReMi-NextGen (https://github.com/VeReMi-dataset/VeReMi-NextGen),
 *   Generator/simulation/mosaic/applications/CamApp/src/main/java/util/SensorErrorModel.java
 * part of a CamApp derived from the Eclipse MOSAIC example applications:
 *   Copyright (c) 2020 Fraunhofer FOKUS and others. All rights reserved.
 *   Made available under the terms of the Eclipse Public License 2.0
 *   (http://www.eclipse.org/legal/epl-2.0). Contact: mosaic@fokus.fraunhofer.de
 *
 * The model itself is unchanged (same distributions, same recursion, same constants):
 *   position : temporally-correlated Gaussian, per-axis initial error E0 ~ U(-5, 5) m,
 *              e_t ~ N((E0 + e_{t-1}) / 2,  0.03 * |E0|)   -> a slowly-wandering GNSS bias
 *   speed    : relative error, factor ~ N(0, 0.00016)      -> v_meas = v * (1 + f)
 *   accel    : d(speed error)/dt over the inter-CAM gap
 *   heading  : initial error ~ U(-20, 20) deg, decaying as exp(-0.1 * v) (bad heading at standstill)
 *
 * Modifications for SCMS-Simulator (also EPL-2.0), all listed here because they are behavioural:
 *   1. DETERMINISM. Upstream uses one process-wide `static final Random random = new Random()`
 *      (unseeded, shared by every vehicle), so no run is reproducible. Here each instance owns a
 *      Random seeded from SHA-256(label | scenario seed | vehicle id) -> "same seed, same dataset".
 *   2. SIM TIME. The acceleration term divides by the inter-sample gap; upstream initialises
 *      `lastMessage = 0`, so the very first call divides by the absolute simulation time (and by
 *      zero at t = 0). Here the first sample reports zero acceleration error. Sampling is driven by
 *      MOSAIC simulation time (ns) only - no wall clock anywhere (cf. the upstream pseudonym timer,
 *      VehicleCamSendingApp.java:59,175,192, which uses LocalDateTime.now(); our pseudonym change
 *      policy is re-based on getSimulationTime() in ScmsBackend).
 *   3. CONFIDENCE, NOT NOISE. Upstream transmits the realised error vector in the CAM
 *      (pos_noise/spd_noise/...). We deliberately do NOT: a receiver could subtract it and recover
 *      the true state, which would breach the MA-visible / ground-truth firewall. The CAM instead
 *      carries an ETSI-style 95% position-confidence radius derived from the error DISTRIBUTION
 *      (posConf below); the realised noise stays on the sender side / in the oracle tables.
 *   3b. ...AND THE CONFIDENCE IS QUANTISED. Not transmitting the noise is not enough: the radius is
 *      computed once from the per-vehicle initial error draw, so a raw value is CONSTANT for the
 *      life of the vehicle and near-unique across the fleet (measured: 62 distinct values over 63
 *      vehicles, one value per vehicle). That makes a legitimately MA-visible field
 *      (ScmsBackend -> CAM posConf -> ma_reports.subject_pos_confidence -> the pos_confidence
 *      column of ml/report_features.csv) a PERFECT cross-pseudonym linkage key: an MA or an ML
 *      model could re-link every pseudonym rotation of a vehicle from it and defeat the
 *      pseudonymity the LA/linkage machinery exists to model. Real receivers report accuracy on a
 *      coarse ladder anyway, so the transmitted radius is snapped UP onto a fleet-shared 1-2-3-5
 *      series ({@link #quantisedConfidence}) -- it still carries accuracy information, but many
 *      vehicles share each rung instead of each owning one.
 *   4. The magnitudes are configurable (SCMS_SENSOR_* env knobs) with upstream values as defaults,
 *      and Pair<> is replaced by an immutable Sample so no extra utility class is needed.
 */
package org.scms.realism;

import java.util.Random;

public final class SensorErrorModel {

    /** One measured (noisy) observation of the vehicle's own state, plus the realised errors. */
    public static final class Sample {
        public final double x;
        public final double y;
        public final double speed;
        public final double heading;
        public final double acceleration;
        public final double xNoise;
        public final double yNoise;
        public final double speedNoise;
        public final double accelNoise;
        public final double headingNoise;
        /** 95% position-confidence radius (m) implied by the error distribution — CAM-transmittable. */
        public final double posConf;

        Sample(double x, double y, double speed, double heading, double acceleration,
               double xNoise, double yNoise, double speedNoise, double accelNoise,
               double headingNoise, double posConf) {
            this.x = x;
            this.y = y;
            this.speed = speed;
            this.heading = heading;
            this.acceleration = acceleration;
            this.xNoise = xNoise;
            this.yNoise = yNoise;
            this.speedNoise = speedNoise;
            this.accelNoise = accelNoise;
            this.headingNoise = headingNoise;
            this.posConf = posConf;
        }
    }

    private final Random rng;
    private final double initialXPositionError;
    private final double initialYPositionError;
    private final double initialSpeedError;
    private final double initialHeadingError;
    private final double positionSigmaFraction;
    private final double headingDecay;
    private final double posConf;

    private double previousXPositionError;
    private double previousYPositionError;
    private double previousSpeedError;
    private double currentSpeedError;
    private long lastSampleNs = Long.MIN_VALUE;

    /** Upstream defaults: 5 m initial position error, N(0, 0.00016) speed factor, 20 deg heading. */
    public SensorErrorModel(long seed) {
        this(seed, 5.0, 0.00016, 20.0, 0.03, 0.1);
    }

    public SensorErrorModel(long seed, double positionErrorM, double speedErrorSd,
                            double headingErrorDeg, double positionSigmaFraction, double headingDecay) {
        this.rng = new Random(seed);
        // Draw order matches upstream's constructor (x, y, speed, heading) so the ported model can be
        // diffed against it sample-for-sample given the same stream.
        this.initialXPositionError = uniform(-positionErrorM, positionErrorM);
        this.initialYPositionError = uniform(-positionErrorM, positionErrorM);
        this.initialSpeedError = gaussian(0.0, speedErrorSd);
        this.initialHeadingError = uniform(-headingErrorDeg, headingErrorDeg);
        this.previousXPositionError = initialXPositionError;
        this.previousYPositionError = initialYPositionError;
        this.previousSpeedError = 0.0;
        this.currentSpeedError = 0.0;
        this.positionSigmaFraction = positionSigmaFraction;
        this.headingDecay = headingDecay;
        // The recursion e_t ~ N((E0 + e_{t-1})/2, 0.03|E0|) has fixed point E0, so the per-axis error
        // magnitude is |E0|; the 2-D 95% radius is 2.448 x the per-axis sigma (sqrt(-2 ln 0.05)),
        // matching how ScmsBackend derives its own posConf. QUANTISED before it can leave this
        // object (see note 3b in the header): the raw value is a per-vehicle constant and would be a
        // cross-pseudonym linkage key in the CAM. The ladder starts at 1 m, which also keeps the old
        // floor (a zero-confidence CAM would be an unrealistic claim of perfect GNSS).
        double axis = Math.sqrt(0.5 * (initialXPositionError * initialXPositionError
                + initialYPositionError * initialYPositionError));
        this.posConf = quantisedConfidence(2.448 * axis);
    }

    /**
     * Coarse, fleet-shared quantisation of a transmitted 95% position-confidence radius (m).
     *
     * <p>ETSI CAMs carry a confidence ellipse whose semi-axis is a coarse reported figure, not the
     * receiver's internal covariance; snapping to a 1-2-3-5 (Renard-style) ladder reproduces that
     * and, crucially, destroys the per-vehicle uniqueness that turns the field into a pseudonym
     * linkage key. The radius is snapped UP so a vehicle never advertises better accuracy than the
     * model actually gives it, and the function is idempotent (a value already on a rung is
     * unchanged), so it is safe to apply at more than one point in the chain.
     */
    public static double quantisedConfidence(double radiusM) {
        double r = (Double.isFinite(radiusM) ? Math.max(0.0, radiusM) : 0.0);
        for (double rung : CONFIDENCE_LADDER_M) {
            if (r <= rung + 1e-9) {
                return rung;
            }
        }
        return CONFIDENCE_LADDER_M[CONFIDENCE_LADDER_M.length - 1];
    }

    /** Reported-accuracy rungs (m). Coarse enough that many vehicles share each one. */
    public static final double[] CONFIDENCE_LADDER_M = {
        1.0, 2.0, 3.0, 5.0, 8.0, 12.0, 20.0, 35.0, 60.0, 100.0, 200.0, 500.0,
    };

    /**
     * One measurement of the vehicle's own true state.
     *
     * @param trueX        true projected x (m)
     * @param trueY        true projected y (m)
     * @param trueSpeed    true speed (m/s)
     * @param trueHeading  true heading (deg)
     * @param trueAccel    true longitudinal acceleration (m/s^2)
     * @param simTimeNs    MOSAIC simulation time (ns) — never wall clock
     */
    public Sample sample(double trueX, double trueY, double trueSpeed, double trueHeading,
                         double trueAccel, long simTimeNs) {
        // --- position: temporally correlated bias
        double muX = (initialXPositionError + previousXPositionError) / 2.0;
        double muY = (initialYPositionError + previousYPositionError) / 2.0;
        double sigmaX = positionSigmaFraction * Math.abs(initialXPositionError);
        double sigmaY = positionSigmaFraction * Math.abs(initialYPositionError);
        double noiseX = gaussian(muX, sigmaX);
        double noiseY = gaussian(muY, sigmaY);
        previousXPositionError = noiseX;
        previousYPositionError = noiseY;

        // --- speed: relative (multiplicative) odometry error
        double correctedSpeed = trueSpeed * (1.0 + initialSpeedError);
        currentSpeedError = trueSpeed - correctedSpeed;

        // --- acceleration: derivative of the speed error over the inter-sample gap
        double accelError = 0.0;
        if (lastSampleNs != Long.MIN_VALUE) {
            double deltaT = (simTimeNs - lastSampleNs) / 1_000_000_000.0;
            if (deltaT > 0) {
                accelError = (currentSpeedError - previousSpeedError) / deltaT;
            }
        }
        lastSampleNs = simTimeNs;
        previousSpeedError = currentSpeedError;

        // --- heading: large at standstill, decaying with speed
        double headingError = initialHeadingError * Math.exp(-headingDecay * trueSpeed);

        return new Sample(trueX + noiseX, trueY + noiseY, correctedSpeed,
                norm360(trueHeading + headingError), trueAccel + accelError,
                noiseX, noiseY, currentSpeedError, accelError, headingError, posConf);
    }

    private double uniform(double min, double max) {
        return min + (max - min) * rng.nextDouble();
    }

    private double gaussian(double mean, double stddev) {
        return mean + stddev * rng.nextGaussian();
    }

    private static double norm360(double a) {
        return ((a % 360) + 360) % 360;
    }
}
