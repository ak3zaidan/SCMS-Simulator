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

    private static final double CAM_INTERVAL_S = envD("SCMS_CAM_INTERVAL", 1.0);   // ETSI T_GenCamMax
    private static final double CAM_MIN_S = envD("SCMS_CAM_MIN", 0.1);             // ETSI T_GenCamMin
    private static final int FLOOD_BURST = envI("SCMS_FLOOD_BURST", 10);           // DoS msgs per tick
    private static final double MOVING_SPEED_MS = 5.0;
    private static final double FROZEN_EPS_M = 0.5;
    private static final int FROZEN_COUNT = envI("SCMS_FROZEN_COUNT", 3);
    private static final double ART_MAX_M = envD("SCMS_ART_MAX_M", 1000.0);
    private static final double STALE_MAX_S = envD("SCMS_STALE_MAX", 5.0);
    private static final int FREQ_MAX = envI("SCMS_FREQ_MAX", 12);
    private static final double SPEED_TOL_M = envD("SCMS_SPEED_TOL", 10.0);
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
    private double lastSentX, lastSentY, lastSentHeading, lastSentSpeed;
    private boolean haveSentBefore = false;
    private String myDigest;
    private ScmsBackend.Cred cred;
    private double selfX, selfY;
    private boolean haveSelf = false;

    private static final double MIN_REF_GAP_S = 0.5;   // motion-consistency baseline (rate-independent)
    private static final double MAX_ACCEL_MS2 = 9.0;   // physical accel/decel bound for plausibility

    private static final class Rx {
        double lastX, lastY, lastT, lastHeading;       // most recent CAM (frequency, sybil, live)
        double refX, refY, refT, refHeading;           // lagged >=0.5 s reference (motion checks)
        boolean hasRef = false;
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
        boolean flood = cred != null && ("DoS".equals(AttackLib.baseOf(cred.attackType))
                || "DoSRandom".equals(AttackLib.baseOf(cred.attackType)));
        // ETSI EN 302 637-2 CAM generation rules: trigger on dynamics (Δpos>4 m / Δhdg>4° /
        // Δspeed>0.5 m/s), floored at T_GenCamMin and heart-beating at T_GenCamMax. DoS floods.
        double dtLast = tS - lastSendS;
        boolean due;
        if (flood) {
            due = true;
        } else if (dtLast < CAM_MIN_S) {
            due = false;
        } else if (dtLast >= CAM_INTERVAL_S || !haveSentBefore) {
            due = true;
        } else {
            double dPos = Math.hypot(selfX - lastSentX, selfY - lastSentY);
            double dHdg = angleDiff(updated.getHeading(), lastSentHeading);
            double dSpd = Math.abs(updated.getSpeed() - lastSentSpeed);
            due = dPos > 4.0 || dHdg > 4.0 || dSpd > 0.5;
        }
        if (!due) {
            return;
        }
        lastSendS = tS;
        lastSentX = selfX; lastSentY = selfY;
        lastSentHeading = updated.getHeading(); lastSentSpeed = updated.getSpeed();
        haveSentBefore = true;
        sendCount++;
        ScmsBackend backend = ScmsBackend.instance();
        cred = backend.beaconCred(getOperatingSystem().getId(), tNs);   // rotates pseudonym if due
        myDigest = cred.certDigest;
        AttackLib.Claim c = backend.claim(getOperatingSystem().getId(), sendCount,
                selfX, selfY, updated.getSpeed(), updated.getHeading(), tNs);
        backend.onCamSent(cred.certDigest, tS);
        send(backend, cred.certDigest, c.x, c.y, c.speed, c.heading, c.posConf, c.genTimeNs);
        if (c.flood) {   // DoS: emit a burst so receivers/channel actually see flooding
            for (int b = 1; b < FLOOD_BURST; b++) {
                backend.onCamSent(cred.certDigest, tS);
                send(backend, cred.certDigest, c.x, c.y, c.speed, c.heading, c.posConf, c.genTimeNs);
            }
        }

        if (c.sybilGhosts > 0) {   // Sybil: emit ghost identities clustered near the attacker
            List<String> ghosts = backend.ghostDigests(getOperatingSystem().getId());
            for (int k = 0; k < ghosts.size(); k++) {
                double gx = c.x + ((k % 2 == 0) ? 1.0 : -1.0);   // tight cluster (~1.5 m): physically implausible
                double gy = c.y + ((k < 2) ? 1.0 : -1.0);
                backend.onCamSent(ghosts.get(k), tS);
                send(backend, ghosts.get(k), gx, gy, c.speed, c.heading, c.posConf, tNs);
            }
        }
    }

    private void send(ScmsBackend backend, String certDigest, double x, double y, double speed,
                      double heading, double posConf, long genTimeNs) {
        MessageRouting routing = getOperatingSystem().getAdHocModule().createMessageRouting()
                .channel(AdHocChannel.CCH).topological().broadcast().singlehop().build();
        getOperatingSystem().getAdHocModule().sendV2xMessage(new SignedCam(routing,
                certDigest, cred.iPeriod, cred.jIndex, cred.linkageValueHex,
                x, y, speed, heading, posConf, genTimeNs, true));
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

        // Motion consistency is evaluated against a lagged reference (>= MIN_REF_GAP old) rather
        // than the immediately-previous CAM, so detection is independent of the CAM rate (which
        // now varies 1-10 Hz under the ETSI generation rules).
        double gapRef = s.hasRef ? (t - s.refT) : 0;
        double movedRef = s.hasRef ? Math.hypot(cam.claimedX - s.refX, cam.claimedY - s.refY) : 0;
        boolean refReady = s.hasRef && gapRef >= MIN_REF_GAP_S;
        if (refReady) {
            s.frozenCount = (cam.claimedSpeed > MOVING_SPEED_MS && movedRef < FROZEN_EPS_M)
                    ? s.frozenCount + 1 : 0;
        }

        // Sybil co-location: many distinct identities claiming ~one 5 m cell within 1.5 s.
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
        } else if (refReady && gapRef <= 5 && movedRef > 50 + 60 * gapRef) {
            reason = "positionJump"; score = movedRef;
        } else if (cellDistinct >= SYBIL_MIN) {
            reason = "sybilCoLocation"; score = cellDistinct;
        } else if (refReady && gapRef <= 1.5) {
            // Physical plausibility: you cannot travel FARTHER than your claimed speed allows over
            // the gap (plus an accel + confidence margin). Moving LESS (braking, turning, a lost
            // CAM) is legitimate, so this is one-sided — which avoids false-flagging normal urban
            // manoeuvres while still catching teleports / random-position lies.
            double maxDist = cam.claimedSpeed * gapRef + 0.5 * MAX_ACCEL_MS2 * gapRef * gapRef
                    + SPEED_TOL_M + cam.posConf;
            if (movedRef > maxDist) {
                reason = "positionSpeedInconsistency"; score = movedRef - maxDist;
            }
        }
        if (reason == null && refReady && movedRef > 20 && gapRef <= 1.0) {
            double bearing = norm360(Math.toDegrees(Math.atan2(cam.claimedX - s.refX, cam.claimedY - s.refY)));
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
        if (!s.hasRef || gapRef >= MIN_REF_GAP_S) {   // advance the lagged reference
            s.refX = cam.claimedX; s.refY = cam.claimedY; s.refT = t; s.refHeading = cam.claimedHeading;
            s.hasRef = true;
        }

        if (reason != null) {
            backend.onDetection(myDigest, dg, score, t, reason, cam.posConf);
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
