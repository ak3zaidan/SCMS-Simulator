/*
 * SPDX-License-Identifier: Apache-2.0
 * Part of the SCMS-aware V2X simulation & global Misbehavior-Authority dataset framework.
 */
package org.scms.app;

import org.eclipse.mosaic.fed.application.app.AbstractApplication;
import org.eclipse.mosaic.fed.application.app.api.VehicleApplication;
import org.eclipse.mosaic.fed.application.app.api.os.VehicleOperatingSystem;
import org.eclipse.mosaic.lib.objects.vehicle.VehicleData;
import org.eclipse.mosaic.lib.util.scheduling.Event;

/**
 * Minimal SCMS-aware vehicle application: the extension seam for the MOSAIC layer.
 *
 * <p>This first version proves the custom-application build/deploy/run path on the
 * MOSAIC 25.x toolchain. It reacts to each SUMO-driven vehicle update — which is
 * exactly where later versions will (1) select the active pseudonym certificate,
 * (2) build and sign the CAM, (3) run local plausibility detection on received
 * messages, and (4) emit ETSI TS 103 759-shaped misbehaviour reports to the SCMS
 * back-end federate. The Python reference in {@code src/scms_sim_ref} defines the
 * validated linkage/report logic this app will mirror.
 */
public class ScmsBeaconApp extends AbstractApplication<VehicleOperatingSystem>
        implements VehicleApplication {

    private int vehicleUpdates = 0;

    @Override
    public void onStartup() {
        getLog().infoSimTime(this, "SCMS beacon app started on unit '{}'",
                getOperatingSystem().getId());
    }

    @Override
    public void onVehicleUpdated(VehicleData previous, VehicleData updated) {
        if (updated == null) {
            return;
        }
        vehicleUpdates++;
        // Log sparsely: the first update and then every 50th, to confirm the app
        // is receiving ground-truth kinematics it will later sign into CAMs.
        if (vehicleUpdates == 1 || vehicleUpdates % 50 == 0) {
            getLog().infoSimTime(this,
                    "unit '{}' update #{}: pos={} speed={} m/s heading={} deg",
                    getOperatingSystem().getId(), vehicleUpdates,
                    updated.getPosition(), updated.getSpeed(), updated.getHeading());
        }
    }

    @Override
    public void onShutdown() {
        getLog().infoSimTime(this, "SCMS beacon app shutdown on unit '{}'; total vehicle updates = {}",
                getOperatingSystem().getId(), vehicleUpdates);
    }

    @Override
    public void processEvent(Event event) {
        // No self-scheduled events yet; CAM triggering + report emission arrive next.
    }
}
