/*
 * SPDX-License-Identifier: Apache-2.0
 * SCMS-aware vehicle application (MOSAIC layer, v5 — full attack + detector suite).
 *
 * Each vehicle broadcasts a signed CAM over ITS-G5 (AdHoc CCH); MOSAIC's SNS radio decides
 * who receives it. Attack behaviour (content, timing, flooding, Sybil ghosts) comes from the
 * back-end via AttackLib. Every receiver runs a distributed detector suite over the CAMs it
 * actually receives and reports suspects to the (in-JVM) MA back-end. Detectors:
 *   staleOrReplay, beaconFrequency, acceptanceRangeThreshold, positionJump, sybilCoLocation,
 *   positionSpeedInconsistency, headingInconsistency, constantPositionFrozen.
 */
package org.scms.app;

import java.util.HashMap;
import java.util.List;
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

import org.scms.attacks.AttackLib;
import org.scms.backend.ScmsBackend;

public class ScmsBeaconApp extends AbstractApplication<VehicleOperatingSystem>
        implements VehicleApplication, CommunicationApplication {

    private static final double CAM_INTERVAL_S = envD("SCMS_CAM_INTERVAL", 1.0);
    private static final double MOVING_SPEED_MS = 5.0;
    private static final double FROZEN_EPS_M = 0.5;
    private static final int FROZEN_COUNT = envI("SCMS_FROZEN_COUNT", 3);
    private static final double ART_MAX_M = envD("SCMS_ART_MAX_M", 1000.0);
    private static final double STALE_MAX_S = envD("SCMS_STALE_MAX", 5.0);
    private static final int FREQ_MAX = envI("SCMS_FREQ_MAX", 6);
    private static final double SPEED_TOL_M = envD("SCMS_SPEED_TOL", 25.0);
    private static final double HEADING_DIFF = envD("SCMS_HEADING_DIFF", 120.0);
    private static final int SYBIL_MIN = envI("SCMS_SYBIL_MIN", 5);

    private static int envI(String n, int d) {
        String e = System.getenv(n);
        try { return (e != null && !e.isBlank()) ? Integer.parseInt(e.trim()) : d; } catch (NumberFormatException ex) { return d; }
    }

    private static double envD(String n, double d) {
        String e = System.getenv(n);
        try { return (e != null && !e.isBlank()) ? Double.parseDouble(e.trim()) : d; } catch (NumberFormatException ex) { return d; }
    }

    private int sendCount = 0;
    private double lastSendS = Double.NEGATIVE_INFINITY;
    private String myDigest;
    private ScmsBackend.Cred cred;
    private double selfX, selfY;
    private boolean haveSelf = false;

    private static final class Rx {
        double lastX, lastY, lastT, lastHeading;
        int frozenCount;
        double winStart;
        int winCount;
        boolean hasPrev = false;
    }

    private final Map<String, Rx> senders = new HashMap<>();
    private final Map<String, Map<String, Double>> sybilGrid = new HashMap<>();  // cell -> (digest -> lastSeen)

    @Override
    public void onStartup() {
        String id = getOperatingSystem().getId();
        ScmsBackend.instance().register(id);
        cred = ScmsBackend.instance().getCredential(id);
        myDigest = cred.certDigest;
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
        selfX = p.getX();
        selfY = p.getY();
        haveSelf = true;
        long tNs = getOperatingSystem().getSimulationTime();
        double tS = tNs / 1e9;
        boolean flood = cred != null && ("DoS".equals(cred.attackType) || "DoSRandom".equals(cred.attackType));
        if (!flood && tS - lastSendS < CAM_INTERVAL_S) {
            return; // throttle genuine/normal senders to ~1 Hz; DoS floods every tick
        }
        lastSendS = tS;
        sendCount++;
        ScmsBackend backend = ScmsBackend.instance();
        AttackLib.Claim c = backend.claim(getOperatingSystem().getId(), sendCount,
                p.getX(), p.getY(), updated.getSpeed(), updated.getHeading(), tNs);
        backend.onCamSent(cred.certDigest, tS);
        send(backend, cred.certDigest, c.x, c.y, c.speed, c.heading, c.genTimeNs);

        if (c.sybilGhosts > 0) {   // Sybil: emit ghost identities clustered near the attacker
            List<String> ghosts = backend.ghostDigests(getOperatingSystem().getId());
            for (int k = 0; k < ghosts.size(); k++) {
                double gx = c.x + ((k % 2 == 0) ? 1.0 : -1.0);   // tight cluster (~1.5 m): physically implausible
                double gy = c.y + ((k < 2) ? 1.0 : -1.0);
                backend.onCamSent(ghosts.get(k), tS);
                send(backend, ghosts.get(k), gx, gy, c.speed, c.heading, tNs);
            }
        }
    }

    private void send(ScmsBackend backend, String certDigest, double x, double y, double speed,
                      double heading, long genTimeNs) {
        MessageRouting routing = getOperatingSystem().getAdHocModule().createMessageRouting()
                .channel(AdHocChannel.CCH).topological().broadcast().singlehop().build();
        getOperatingSystem().getAdHocModule().sendV2xMessage(new SignedCam(routing,
                certDigest, cred.iPeriod, cred.jIndex, cred.linkageValueHex,
                x, y, speed, heading, genTimeNs, true));
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
        if (backend.isRevoked(dg, t)) {
            return; // ENFORCEMENT: drop revoked certificates
        }
        if (!cam.sigValid) {
            return;
        }

        Rx s = senders.get(dg);
        double gap = (s == null) ? 0 : (t - s.lastT);
        double moved = (s == null) ? 0 : Math.hypot(cam.claimedX - s.lastX, cam.claimedY - s.lastY);
        if (s == null) {
            s = new Rx();
            s.winStart = t;
            senders.put(dg, s);
        }
        if (t - s.winStart >= 1.0) {
            s.winStart = t;
            s.winCount = 1;
        } else {
            s.winCount++;
        }
        if (s.hasPrev) {
            s.frozenCount = (cam.claimedSpeed > MOVING_SPEED_MS && moved < FROZEN_EPS_M) ? s.frozenCount + 1 : 0;
        }

        // Sybil co-location: many distinct identities claiming ~one 20 m cell within 3 s.
        String cell = ((long) Math.floor(cam.claimedX / 5)) + ":" + ((long) Math.floor(cam.claimedY / 5));
        Map<String, Double> cd = sybilGrid.computeIfAbsent(cell, k -> new HashMap<>());
        cd.put(dg, t);
        cd.values().removeIf(v -> t - v > 1.5);
        int cellDistinct = cd.size();

        double staleSec = t - cam.genTimeNs / 1e9;
        double artDist = haveSelf ? Math.hypot(cam.claimedX - selfX, cam.claimedY - selfY) : 0;

        String reason = null;
        double score = 0;
        if (staleSec > STALE_MAX_S) {
            reason = "staleOrReplay"; score = staleSec;
        } else if (s.winCount > FREQ_MAX) {
            reason = "beaconFrequency"; score = s.winCount;
        } else if (haveSelf && artDist > ART_MAX_M) {
            reason = "acceptanceRangeThreshold"; score = artDist;
        } else if (s.hasPrev && gap > 0 && gap <= 5 && moved > 50 + 60 * gap) {
            reason = "positionJump"; score = moved;
        } else if (cellDistinct >= SYBIL_MIN) {
            reason = "sybilCoLocation"; score = cellDistinct;
        } else if (s.hasPrev && gap >= 0.5 && gap <= 3) {
            double expected = cam.claimedSpeed * gap;
            double diff = Math.abs(moved - expected);
            if (diff > Math.max(SPEED_TOL_M, 0.8 * expected)) {
                reason = "positionSpeedInconsistency"; score = diff;
            }
        }
        if (reason == null && s.hasPrev && moved > 10 && gap >= 0.5 && gap <= 3) {
            double bearing = norm360(Math.toDegrees(Math.atan2(cam.claimedX - s.lastX, cam.claimedY - s.lastY)));
            double hd = angleDiff(cam.claimedHeading, bearing);
            if (hd > HEADING_DIFF) {
                reason = "headingInconsistency"; score = hd;
            }
        }
        if (reason == null && s.frozenCount >= FROZEN_COUNT) {
            reason = "constantPositionFrozen"; score = cam.claimedSpeed;
        }

        s.lastX = cam.claimedX;
        s.lastY = cam.claimedY;
        s.lastT = t;
        s.lastHeading = cam.claimedHeading;
        s.hasPrev = true;

        if (reason != null) {
            backend.onDetection(myDigest, dg, score, t, reason);
        }
    }

    private static double norm360(double a) {
        return ((a % 360) + 360) % 360;
    }

    private static double angleDiff(double a, double b) {
        double d = Math.abs(norm360(a) - norm360(b));
        return d > 180 ? 360 - d : d;
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
    }

    @Override
    public void processEvent(Event event) {
    }
}
