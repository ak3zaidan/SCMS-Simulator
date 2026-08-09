/*
 * SPDX-License-Identifier: Apache-2.0
 * Part of the SCMS-aware V2X simulation & global Misbehavior-Authority dataset framework.
 */
package org.scms.app;

import org.eclipse.mosaic.fed.application.app.AbstractApplication;
import org.eclipse.mosaic.fed.application.app.api.VehicleApplication;
import org.eclipse.mosaic.fed.application.app.api.os.VehicleOperatingSystem;
import org.eclipse.mosaic.lib.geo.CartesianPoint;
import org.eclipse.mosaic.lib.objects.vehicle.VehicleData;
import org.eclipse.mosaic.lib.util.scheduling.Event;

import org.scms.backend.ScmsBackend;

/**
 * SCMS-aware vehicle application (MOSAIC layer, v1).
 *
 * Registers the vehicle with the {@link ScmsBackend} (which provisions a pseudonym
 * certificate carrying a real CAMP-SCP2 linkage value) and forwards every
 * SUMO-driven kinematic update to the back-end, which models CAM reception + local
 * detection, correlates misbehaviour reports, resolves identity via the two Linkage
 * Authorities (without learning it), revokes, issues a CRL, and enforces it — writing
 * the MA-visible + ground-truth dataset. This mirrors the validated Python reference.
 */
public class ScmsBeaconApp extends AbstractApplication<VehicleOperatingSystem>
        implements VehicleApplication {

    @Override
    public void onStartup() {
        ScmsBackend.instance().register(getOperatingSystem().getId());
        getLog().infoSimTime(this, "SCMS app active on unit '{}'", getOperatingSystem().getId());
    }

    @Override
    public void onVehicleUpdated(VehicleData previous, VehicleData updated) {
        if (updated == null) {
            return;
        }
        CartesianPoint p = updated.getProjectedPosition();
        if (p == null) {
            return;
        }
        ScmsBackend.instance().onVehicleUpdate(
                getOperatingSystem().getId(),
                getOperatingSystem().getSimulationTime(),
                p.getX(), p.getY(),
                updated.getSpeed(), updated.getHeading());
    }

    @Override
    public void onShutdown() {
        // Dataset is flushed once at JVM exit by ScmsBackend's shutdown hook.
    }

    @Override
    public void processEvent(Event event) {
        // No self-scheduled events in v1.
    }
}
