/*
 * SPDX-License-Identifier: Apache-2.0
 * SCMS-aware vehicle application (MOSAIC layer, v2 — real AdHoc reception).
 *
 * Each vehicle broadcasts a signed CAM over ITS-G5 (AdHoc CCH); MOSAIC's radio model
 * (SNS: range + delay) decides who actually receives it. Detection is now genuinely
 * DISTRIBUTED: every receiver runs a local position-speed-consistency check on the
 * CAMs it truly received and, on a suspect, files a report with the (in-JVM) MA
 * back-end, which correlates, resolves identity via the two Linkage Authorities
 * (without learning it), revokes, and issues a CRL that receivers then enforce.
 */
package org.scms.app;

import java.util.HashMap;
import java.util.Map;

import org.eclipse.mosaic.fed.application.ambassador.simulation.communication.AdHocModuleConfiguration;
import org.eclipse.mosaic.fed.application.ambassador.simulation.communication.CamBuilder;
import org.eclipse.mosaic.fed.application.ambassador.simulation.communication.ReceivedAcknowledgement;
import org.eclipse.mosaic.fed.application.ambassador.simulation.communication.ReceivedV2xMessage;
import org.eclipse.mosaic.fed.application.app.AbstractApplication;
import org.eclipse.mosaic.fed.application.app.api.CommunicationApplication;
import org.eclipse.mosaic.fed.application.app.api.VehicleApplication;
import org.eclipse.mosaic.fed.application.app.api.os.VehicleOperatingSystem;
import org.eclipse.mosaic.interactions.communication.V2xMessageTransmission;
import org.eclipse.mosaic.lib.enums.AdHocChannel;
import org.eclipse.mosaic.lib.geo.CartesianPoint;
import org.eclipse.mosaic.lib.objects.v2x.MessageRouting;
import org.eclipse.mosaic.lib.objects.vehicle.VehicleData;
import org.eclipse.mosaic.lib.util.scheduling.Event;

import org.scms.backend.ScmsBackend;

public class ScmsBeaconApp extends AbstractApplication<VehicleOperatingSystem>
        implements VehicleApplication, CommunicationApplication {

    private static final double CONSISTENCY_THRESH_M = 5.0;

    private int updateCount = 0;
    private String myDigest;
    private final Map<String, double[]> lastClaimed = new HashMap<>(); // subjectDigest -> {x, y, t}

    @Override
    public void onStartup() {
        String id = getOperatingSystem().getId();
        ScmsBackend.instance().register(id);
        myDigest = ScmsBackend.instance().getCredential(id).certDigest;
        getOperatingSystem().getAdHocModule().enable(new AdHocModuleConfiguration()
                .addRadio().channel(AdHocChannel.CCH).power(50).create());
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
        updateCount++;
        String id = getOperatingSystem().getId();
        ScmsBackend backend = ScmsBackend.instance();
        ScmsBackend.Cred cred = backend.getCredential(id);
        double[] claimed = backend.claimedPosition(id, updateCount, p.getX(), p.getY());
        long tNs = getOperatingSystem().getSimulationTime();
        backend.onCamSent(cred.certDigest, tNs / 1e9);

        MessageRouting routing = getOperatingSystem().getAdHocModule().createMessageRouting()
                .channel(AdHocChannel.CCH).topological().broadcast().singlehop().build();
        getOperatingSystem().getAdHocModule().sendV2xMessage(new SignedCam(routing,
                cred.certDigest, cred.iPeriod, cred.jIndex, cred.linkageValueHex,
                claimed[0], claimed[1], updated.getSpeed(), updated.getHeading(), tNs, true));
    }

    @Override
    public void onMessageReceived(ReceivedV2xMessage rx) {
        if (!(rx.getMessage() instanceof SignedCam)) {
            return;
        }
        SignedCam cam = (SignedCam) rx.getMessage();
        if (cam.senderCertDigest.equals(myDigest)) {
            return; // ignore our own broadcast
        }
        double t = getOperatingSystem().getSimulationTime() / 1e9;
        ScmsBackend backend = ScmsBackend.instance();
        if (backend.isRevoked(cam.senderCertDigest, t)) {
            return; // ENFORCEMENT: drop CAMs from revoked certificates
        }
        if (!cam.sigValid) {
            return; // invalid-signature handling is a later attack class
        }
        double[] prev = lastClaimed.get(cam.senderCertDigest);
        lastClaimed.put(cam.senderCertDigest, new double[] {cam.claimedX, cam.claimedY, t});
        if (prev == null) {
            return;
        }
        double moved = Math.hypot(cam.claimedX - prev[0], cam.claimedY - prev[1]);
        double inconsistency = Math.abs(moved - cam.claimedSpeed * (t - prev[2]));
        if (inconsistency > CONSISTENCY_THRESH_M) {
            backend.onDetection(myDigest, cam.senderCertDigest, inconsistency, t);
        }
    }

    @Override
    public void onAcknowledgementReceived(ReceivedAcknowledgement acknowledgement) {
        // not used
    }

    @Override
    public void onCamBuilding(CamBuilder camBuilder) {
        // we broadcast our own SignedCam; no built-in CAM assembly needed
    }

    @Override
    public void onMessageTransmitted(V2xMessageTransmission transmission) {
        // not used
    }

    @Override
    public void onShutdown() {
        // dataset flushed once at JVM exit by ScmsBackend's shutdown hook
    }

    @Override
    public void processEvent(Event event) {
        // no self-scheduled events
    }
}
