/*
 * SPDX-License-Identifier: Apache-2.0
 * SCMS-aware vehicle application (MOSAIC layer, v3 — robust ConstPos detector).
 *
 * Each vehicle broadcasts a signed CAM (~1 Hz) over ITS-G5 (AdHoc CCH); MOSAIC's
 * radio model (SNS: range + delay) decides who receives it. Detection is distributed:
 * each receiver watches, per sender, whether the CLAIMED position stays frozen across
 * several consecutive CAMs while the sender still claims to be moving — the signature
 * of a constant-position falsification. This is robust to real urban stop-and-go
 * traffic (legit vehicles' claimed positions always change; a stopped legit vehicle
 * claims ~0 speed), unlike an instantaneous speed-vs-displacement check which also
 * flags normal braking. Suspects are reported to the (in-JVM) MA back-end, which
 * correlates, resolves via the two Linkage Authorities (without learning the identity),
 * revokes, and issues a CRL that receivers enforce.
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

    private static final double CAM_INTERVAL_S = 1.0;   // ~1 Hz beaconing
    private static final double MOVING_SPEED_MS = 5.0;   // "claims to be moving" threshold
    private static final double FROZEN_EPS_M = 0.5;      // claimed position considered unchanged
    private static final int FROZEN_COUNT = 3;           // consecutive frozen CAMs -> suspect

    private int sendCount = 0;
    private double lastSendS = Double.NEGATIVE_INFINITY;
    private String myDigest;
    // per sender: {lastClaimedX, lastClaimedY, consecutiveFrozenCount}
    private final Map<String, double[]> senderState = new HashMap<>();

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
        long tNs = getOperatingSystem().getSimulationTime();
        double tS = tNs / 1e9;
        if (tS - lastSendS < CAM_INTERVAL_S) {
            return; // throttle to ~1 Hz
        }
        lastSendS = tS;
        sendCount++;
        String id = getOperatingSystem().getId();
        ScmsBackend backend = ScmsBackend.instance();
        ScmsBackend.Cred cred = backend.getCredential(id);
        double[] claimed = backend.claimedPosition(id, sendCount, p.getX(), p.getY());
        backend.onCamSent(cred.certDigest, tS);

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
            return;
        }
        double t = getOperatingSystem().getSimulationTime() / 1e9;
        ScmsBackend backend = ScmsBackend.instance();
        if (backend.isRevoked(cam.senderCertDigest, t)) {
            return; // ENFORCEMENT: drop CAMs from revoked certificates
        }
        if (!cam.sigValid) {
            return;
        }
        double[] st = senderState.get(cam.senderCertDigest);
        if (st == null) {
            senderState.put(cam.senderCertDigest, new double[] {cam.claimedX, cam.claimedY, 0});
            return;
        }
        double moved = Math.hypot(cam.claimedX - st[0], cam.claimedY - st[1]);
        double frozenCount = st[2];
        if (cam.claimedSpeed > MOVING_SPEED_MS && moved < FROZEN_EPS_M) {
            frozenCount += 1;                 // claims to move, but claimed position didn't change
        } else {
            frozenCount = 0;                  // legit motion (or genuinely stopped) resets
        }
        senderState.put(cam.senderCertDigest, new double[] {cam.claimedX, cam.claimedY, frozenCount});
        if (frozenCount >= FROZEN_COUNT) {
            backend.onDetection(myDigest, cam.senderCertDigest, cam.claimedSpeed, t);
        }
    }

    @Override
    public void onAcknowledgementReceived(ReceivedAcknowledgement acknowledgement) {
        // not used
    }

    @Override
    public void onCamBuilding(CamBuilder camBuilder) {
        // we broadcast our own SignedCam
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
