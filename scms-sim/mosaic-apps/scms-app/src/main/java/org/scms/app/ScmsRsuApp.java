/*
 * SPDX-License-Identifier: Apache-2.0
 * SCMS road-side unit: a static, always-trusted misbehaviour observer.
 *
 * Why this class exists at all: MOSAIC assigns applications to UNITS, and an application typed
 * AbstractApplication<VehicleOperatingSystem> (ScmsBeaconApp) cannot be attached to an RSU unit --
 * MOSAIC fails at unit start-up. That is why every generated scenario used to strip `rsus` from the
 * mapping wholesale, which in turn meant the MOSAIC path had NO infrastructure evidence at all,
 * while the pure-Python engine and the feature builder both already modelled it.
 *
 * An RSU here is deliberately a RECEIVER ONLY: it never beacons, never carries a driver or an
 * attack role, and never appears in gt_vehicle / gt_identity_map (see ScmsBackend.rsuCredential).
 * It runs the SAME detector suite as a vehicle (CamDetector) over the CAMs its antenna actually
 * receives, from a fixed, surveyed position -- which is the whole point of infrastructure evidence:
 * a static receiver with a known-good position is not fooled by the mobility a mobile witness has
 * to guess at, and it cannot itself be an attacker or a colluder.
 *
 * Channel realism is not merely a mirror of the vehicle receiver: since Phase 2 both run the SAME
 * org.scms.radio.RxChannel instance type (weather attenuation, LOS/NLOSb obstruction computed from
 * the sender's TRUE position via the back-end oracle, CSMA/CA congestion loss), so RSU links are
 * neither unrealistically perfect relative to vehicle links nor governed by a second, drifting copy
 * of the radio. An RSU also senses the channel for the DCC channel-busy-ratio estimate even though
 * it never transmits a CAM of its own.
 *
 * Placement, count and the WGS-84 positions come from the scenario generator
 * (mapgen.rsu_units / SCMS_RSUS / SCMS_RSU_PLACEMENT); this class only has to be present in the
 * jar for gen_scenario to start emitting RSU units at all.
 */
package org.scms.app;

import org.eclipse.mosaic.fed.application.ambassador.simulation.communication.AdHocModuleConfiguration;
import org.eclipse.mosaic.fed.application.ambassador.simulation.communication.CamBuilder;
import org.eclipse.mosaic.fed.application.ambassador.simulation.communication.ReceivedAcknowledgement;
import org.eclipse.mosaic.fed.application.ambassador.simulation.communication.ReceivedV2xMessage;
import org.eclipse.mosaic.fed.application.app.AbstractApplication;
import org.eclipse.mosaic.fed.application.app.api.CommunicationApplication;
import org.eclipse.mosaic.fed.application.app.api.os.RoadSideUnitOperatingSystem;
import org.eclipse.mosaic.interactions.communication.V2xMessageTransmission;
import org.eclipse.mosaic.lib.enums.AdHocChannel;
import org.eclipse.mosaic.lib.geo.CartesianPoint;
import org.eclipse.mosaic.lib.geo.GeoPoint;
import org.eclipse.mosaic.lib.util.scheduling.Event;

import org.scms.backend.ScmsBackend;
import org.scms.radio.RxChannel;

public class ScmsRsuApp extends AbstractApplication<RoadSideUnitOperatingSystem>
        implements CommunicationApplication {

    private final CamDetector detector = new CamDetector();
    private String myDigest;
    private double selfX, selfY;
    private boolean haveSelf = false;
    private RxChannel channel;

    @Override
    public void onStartup() {
        String id = getOperatingSystem().getId();
        ScmsBackend backend = ScmsBackend.instance();
        myDigest = backend.rsuCredential(id).certDigest;
        channel = new RxChannel(id);
        try {
            // Idempotent (one parse per JVM). An RSU can start before any vehicle does, so it must
            // be able to bring the footprints up itself rather than depend on ScmsBeaconApp.
            RxChannel.buildings(org.eclipse.mosaic.fed.application.ambassador.SimulationKernel
                    .SimulationKernel.getConfigurationPath());
        } catch (Throwable ex) {
            getLog().warn("building footprints not loaded: {}", ex.toString());
        }
        // A road-side unit is surveyed: its position is known exactly and never changes, so it is
        // resolved once at start-up rather than tracked from mobility updates.
        GeoPoint pos = getOperatingSystem().getPosition();
        if (pos != null) {
            CartesianPoint c = pos.toCartesian();
            if (c != null) {
                selfX = c.getX();
                selfY = c.getY();
                haveSelf = true;
            }
        }
        if (!haveSelf) {
            // Without a position the range check cannot run; every other detector still can, so
            // degrade instead of failing (and say so, because it points at a mapping error).
            getLog().warn("RSU {} has no resolvable position; acceptanceRangeThreshold disabled", id);
        }
        getOperatingSystem().getAdHocModule().enable(new AdHocModuleConfiguration()
                .addRadio().channel(AdHocChannel.CCH).power(50).create());
        getLog().info("SCMS RSU app up: {} at ({}, {})", id, selfX, selfY);
    }

    @Override
    public void onMessageReceived(ReceivedV2xMessage rx) {
        if (!(rx.getMessage() instanceof SignedCam)) {
            return;
        }
        SignedCam cam = (SignedCam) rx.getMessage();
        String dg = cam.senderCertDigest;
        if (dg.equals(myDigest)) {
            return;
        }
        double t = getOperatingSystem().getSimulationTime() / 1e9;
        ScmsBackend backend = ScmsBackend.instance();
        // Weather / LOS-NLOSb obstruction / contention, from the shared model. All geometry comes
        // from the TRUE sender position (channel physics only — see ScmsBeaconApp for why a claimed
        // position must never steer reception probability).
        if (!channel.deliver(dg, t, selfX, selfY, haveSelf)) {
            return;
        }
        if (backend.isRevoked(dg, t)) {
            return;
        }
        if (!cam.sigValid) {
            return;
        }
        CamDetector.Detection d = detector.evaluate(dg, cam, t, selfX, selfY, haveSelf);
        if (d != null) {
            backend.onDetection(myDigest, dg, d.score, t, d.reason, cam.posConf, d.scoreNorm, d.detNorms);
        }
    }

    @Override
    public void onAcknowledgementReceived(ReceivedAcknowledgement acknowledgement) {
    }

    @Override
    public void onCamBuilding(CamBuilder camBuilder) {
    }

    @Override
    public void onMessageTransmitted(V2xMessageTransmission transmission) {
    }

    @Override
    public void onShutdown() {
        if (channel != null) {
            channel.publishStats();
        }
    }

    @Override
    public void processEvent(Event event) {
    }
}
