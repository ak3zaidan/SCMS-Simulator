/*
 * SPDX-License-Identifier: EPL-2.0
 *
 * Ported from VeReMi-NextGen (https://github.com/VeReMi-dataset/VeReMi-NextGen),
 *   Generator/simulation/mosaic/applications/CamApp/src/main/java/entities/DriverProfile.java
 * which is derived from the Eclipse MOSAIC example applications:
 *   Copyright (c) 2020 Fraunhofer FOKUS and others. All rights reserved.
 *   Made available under the terms of the Eclipse Public License 2.0
 *   (http://www.eclipse.org/legal/epl-2.0). Contact: mosaic@fokus.fraunhofer.de
 *
 * Modifications for SCMS-Simulator (also EPL-2.0):
 *   - the 10/80/10 draw is keyed DETERMINISTICALLY on (scenario seed, vehicle id) instead of
 *     `new Random()` (upstream VehicleCamSendingApp.java:72-88), so a run is reproducible;
 *   - the aggressive/passive shares are configurable (SCMS_DRIVER_AGGRESSIVE_PCT / _PASSIVE_PCT);
 *   - parameter values are unchanged from upstream (see the enum constants below).
 *
 * The profile is applied through MOSAIC's public requestVehicleParametersUpdate() API, i.e. it
 * re-parameterises the SUMO car-following/lane-change model per vehicle at runtime — restoring the
 * driver heterogeneity that gen_scenario.py's uniform SCMS_VEH_* prototype overwrite destroys.
 */
package org.scms.realism;

import org.eclipse.mosaic.lib.enums.LaneChangeMode;
import org.eclipse.mosaic.lib.enums.SpeedMode;

public enum DriverProfile {

    AGGRESSIVE(0.5, 3.1, 5.0, 1.1, 0.5, 2.0, LaneChangeMode.AGGRESSIVE, SpeedMode.AGGRESSIVE),
    NORMAL(1.0, 2.6, 4.5, 1.0, 0.5, 2.5, LaneChangeMode.DEFAULT, SpeedMode.NORMAL),
    PASSIVE(1.5, 2.1, 4.0, 0.9, 0.5, 3.0, LaneChangeMode.CAUTIOUS, SpeedMode.CAUTIOUS);

    private final double tau;
    private final double accel;
    private final double decel;
    private final double speedFactor;
    private final double sigma;
    private final double minGap;
    private final LaneChangeMode laneChangeMode;
    private final SpeedMode speedMode;

    DriverProfile(double tau, double accel, double decel, double speedFactor, double sigma,
                  double minGap, LaneChangeMode laneChangeMode, SpeedMode speedMode) {
        this.tau = tau;
        this.accel = accel;
        this.decel = decel;
        this.speedFactor = speedFactor;
        this.sigma = sigma;
        this.minGap = minGap;
        this.laneChangeMode = laneChangeMode;
        this.speedMode = speedMode;
    }

    public double getTau() {
        return tau;
    }

    public double getAccel() {
        return accel;
    }

    public double getDecel() {
        return decel;
    }

    public double getSpeedFactor() {
        return speedFactor;
    }

    public double getSigma() {
        return sigma;
    }

    public double getMinGap() {
        return minGap;
    }

    public LaneChangeMode getLaneChangeMode() {
        return laneChangeMode;
    }

    public SpeedMode getSpeedMode() {
        return speedMode;
    }

    /**
     * Upstream's 10/80/10 mix, but driven by a caller-supplied deterministic U[0,1) draw so the
     * fleet composition is a pure function of (scenario seed, vehicle id).
     *
     * @param u              deterministic uniform draw in [0,1)
     * @param aggressiveFrac share of AGGRESSIVE drivers (upstream: 0.10)
     * @param passiveFrac    share of PASSIVE drivers (upstream: 0.10)
     */
    public static DriverProfile pick(double u, double aggressiveFrac, double passiveFrac) {
        double agg = Math.max(0.0, Math.min(1.0, aggressiveFrac));
        double pas = Math.max(0.0, Math.min(1.0 - agg, passiveFrac));
        if (u < agg) {
            return AGGRESSIVE;
        }
        return (u < 1.0 - pas) ? NORMAL : PASSIVE;
    }
}
