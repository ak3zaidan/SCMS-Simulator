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
    private static final int CHAN_CAPACITY = envI("SCMS_CHAN_CAPACITY", 25);   // CAMs/100ms before congestion loss (0 = off)
    private static final double CHAN_WINDOW_S = 0.1;
    private static final double WEATHER_DROP = weatherDrop();                   // 802.11p attenuation by weather
    private static final double NLOS_INTENSITY = envD("SCMS_NLOS", 0.0);        // building-obstruction loss (0 = off)

    private static double weatherDrop() {
        String w = System.getenv("SCMS_WEATHER");
        if (w == null) { return 0.0; }
        switch (w.toLowerCase()) {
            case "rain": return 0.05;
            case "fog":  return 0.03;
            case "snow": return 0.10;
            default:     return 0.0;
        }
    }

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
    private static final int MIN_CONSEC = envI("SCMS_MIN_CONSEC", 2);   // consecutive violations before flagging
    private static final double MAX_PLAUSIBLE_ACCEL = envD("SCMS_MAX_ACCEL", 12.0);   // m/s^2 (ETSI/F2MD accel check)
    private static final double KF_ALPHA = 0.5, KF_BETA = 0.3;                        // alpha-beta tracker gains
    private static final double KF_THRESH = envD("SCMS_KF_THRESH", 4.0);             // normalized-residual gate

    private static final class Rx {
        double lastX, lastY, lastT, lastHeading;       // most recent CAM (frequency, sybil, live)
        double refX, refY, refT, refHeading, refSpeed;  // lagged >=0.5 s reference (motion checks)
        boolean hasRef = false;
        int frozenCount;
        int psiStreak, hdgStreak, kfStreak;    // consecutive motion-inconsistency violations
        double kfX, kfY, kfVx, kfVy, kfLastT;   // constant-velocity (alpha-beta) tracker state
        boolean kfInit = false;
        double winStart;
        int winCount;
        boolean hasPrev = false;
    }

    private final Map<String, Rx> senders = new HashMap<>();
    private final Map<String, Map<String, Double>> sybilGrid = new HashMap<>();  // cell -> (digest -> lastSeen)
    private double chanWinStart = Double.NEGATIVE_INFINITY;
    private int chanCount = 0, chanLoad = 0;      // local channel load (CBR proxy)
    private java.util.Random chanRng;             // deterministic contention-loss RNG

    @Override
    public void onStartup() {
        String id = getOperatingSystem().getId();
        ScmsBackend.instance().register(id);
        cred = ScmsBackend.instance().getCredential(id);
        myDigest = cred.certDigest;
        chanRng = new java.util.Random(0x9E3779B97F4A7C15L ^ (long) id.hashCode());
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
        backend.maybeCollude(getOperatingSystem().getId(), tNs);   // colluders file false accusations
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
        // Weather (rain/fog/snow) attenuates the 802.11p link — a fraction of frames are lost.
        if (WEATHER_DROP > 0 && chanRng.nextDouble() < WEATHER_DROP) {
            return;
        }
        // NLOS: buildings obstruct more-distant links in urban areas — loss grows with distance.
        if (NLOS_INTENSITY > 0 && haveSelf) {
            double dist = Math.hypot(cam.claimedX - selfX, cam.claimedY - selfY);
            double pn = NLOS_INTENSITY * Math.min(1.0, Math.max(0.0, (dist - 150.0) / 300.0));
            if (pn > 0 && chanRng.nextDouble() < pn) {
                return;
            }
        }
        // Channel congestion (CSMA/CA contention / CBR collapse): under high local channel load a
        // fraction of frames are lost before they can be decoded. This is the main radio realism a
        // full 802.11p PHY/MAC (OMNeT++/ns-3) would add over SNS — it makes DoS floods actually
        // degrade nearby reception and gives dense traffic a realistic packet-delivery ratio.
        if (t - chanWinStart >= CHAN_WINDOW_S) {
            chanWinStart = t; chanLoad = chanCount; chanCount = 0;
        }
        chanCount++;
        if (CHAN_CAPACITY > 0 && chanLoad > CHAN_CAPACITY) {
            double pDrop = Math.min(0.95, (double) (chanLoad - CHAN_CAPACITY) / CHAN_CAPACITY);
            if (chanRng.nextDouble() < pDrop) {
                return; // frame lost to channel contention
            }
        }
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

        // Motion-consistency candidates on the lagged reference. Physical plausibility: you cannot
        // travel FARTHER than your claimed speed allows over the gap (+ accel + confidence margin);
        // moving LESS (braking, turning, a lost CAM) is legitimate, so it is one-sided.
        double maxDist = 0, hd = 0;
        boolean psiViol = false, hdgViol = false;
        if (refReady && gapRef <= 1.5) {
            maxDist = cam.claimedSpeed * gapRef + 0.5 * MAX_ACCEL_MS2 * gapRef * gapRef
                    + SPEED_TOL_M + cam.posConf;
            psiViol = movedRef > maxDist;
        }
        if (refReady && movedRef > 20 && gapRef <= 1.0) {
            double bearing = norm360(Math.toDegrees(Math.atan2(cam.claimedX - s.refX, cam.claimedY - s.refY)));
            hd = angleDiff(cam.claimedHeading, bearing);
            hdgViol = hd > HEADING_DIFF;
        }
        // Consecutive-violation gating: a single GPS outlier / fault glitch is ONE sample, but a
        // real kinematic attack persists across references. Requiring a streak removes the bulk of
        // outlier- and fault-driven false positives (which otherwise dominate this detector).
        if (refReady) {
            s.psiStreak = psiViol ? s.psiStreak + 1 : 0;
            s.hdgStreak = hdgViol ? s.hdgStreak + 1 : 0;
        }

        // Model-based (constant-velocity alpha-beta / Kalman-like) consistency SCORE. Predict this
        // CAM's position from the tracked position+velocity and normalize the residual by the plausible
        // uncertainty (confidence + unmodelled acceleration). This is a SOFT anomaly score carried in
        // the fusion fingerprint (detnorm_kalmanConsistency) — NOT a hard trigger, because a CV tracker
        // false-positives on sustained curving; an ML fusion model can weight it. The estimate is not
        // updated toward a violating sample (so a spike can't poison the track); it re-inits if stale.
        double kfNorm = 0;
        boolean kfViol = false;
        if (!s.kfInit) {
            s.kfInit = true; s.kfX = cam.claimedX; s.kfY = cam.claimedY; s.kfVx = 0; s.kfVy = 0; s.kfLastT = t;
        } else {
            double dtk = t - s.kfLastT;
            if (dtk <= 0 || dtk > 5) {   // stale / out-of-order: re-init the track
                s.kfX = cam.claimedX; s.kfY = cam.claimedY; s.kfVx = 0; s.kfVy = 0; s.kfLastT = t; s.kfStreak = 0;
            } else {
                double px = s.kfX + s.kfVx * dtk, py = s.kfY + s.kfVy * dtk;
                double resx = cam.claimedX - px, resy = cam.claimedY - py;
                double sigma = cam.posConf + 0.5 * MAX_ACCEL_MS2 * dtk * dtk + 1.0;
                kfNorm = Math.hypot(resx, resy) / sigma;
                kfViol = kfNorm > KF_THRESH;
                s.kfStreak = kfViol ? s.kfStreak + 1 : 0;
                if (!kfViol) {           // update the track only on a consistent sample
                    s.kfX = px + KF_ALPHA * resx; s.kfY = py + KF_ALPHA * resy;
                    s.kfVx += KF_BETA * resx / dtk; s.kfVy += KF_BETA * resy / dtk;
                    s.kfLastT = t;
                } else if (s.kfStreak >= 4) {   // persistent new course: re-baseline to keep tracking
                    s.kfX = cam.claimedX; s.kfY = cam.claimedY; s.kfVx = 0; s.kfVy = 0;
                    s.kfLastT = t; s.kfStreak = 0;
                }
            }
        }

        // A normalized score (~>=1 at the detector's threshold) so it is comparable across
        // detectors, unlike the raw score whose units differ (metres / degrees / seconds / counts).
        String reason = null;
        double score = 0, scoreNorm = 0;
        if (staleSec > STALE_MAX_S) {
            reason = "staleOrReplay"; score = staleSec; scoreNorm = staleSec / STALE_MAX_S;
        } else if (s.winCount > FREQ_MAX) {
            reason = "beaconFrequency"; score = s.winCount; scoreNorm = (double) s.winCount / FREQ_MAX;
        } else if (haveSelf && artDist > ART_MAX_M) {
            reason = "acceptanceRangeThreshold"; score = artDist; scoreNorm = artDist / ART_MAX_M;
        } else if (refReady && gapRef <= 5 && movedRef > 50 + 60 * gapRef) {
            reason = "positionJump"; score = movedRef; scoreNorm = movedRef / (50 + 60 * gapRef);
        } else if (cellDistinct >= SYBIL_MIN) {
            reason = "sybilCoLocation"; score = cellDistinct; scoreNorm = (double) cellDistinct / SYBIL_MIN;
        } else if (psiViol && s.psiStreak >= MIN_CONSEC) {
            reason = "positionSpeedInconsistency"; score = movedRef - maxDist; scoreNorm = movedRef / maxDist;
        } else if (hdgViol && s.hdgStreak >= MIN_CONSEC) {
            reason = "headingInconsistency"; score = hd; scoreNorm = hd / HEADING_DIFF;
        } else if (refReady && gapRef <= 3
                && Math.abs(cam.claimedSpeed - s.refSpeed) / gapRef > MAX_PLAUSIBLE_ACCEL) {
            // implied acceleration exceeds physical limits (ETSI/F2MD accel-plausibility check) —
            // catches jumpy claimed-speed attacks (RandomSpeed, StopAndGo). One-sided => low FP.
            double acc = Math.abs(cam.claimedSpeed - s.refSpeed) / gapRef;
            reason = "implausibleAcceleration"; score = acc; scoreNorm = acc / MAX_PLAUSIBLE_ACCEL;
        } else if (s.frozenCount >= FROZEN_COUNT) {
            reason = "constantPositionFrozen"; score = cam.claimedSpeed; scoreNorm = (double) s.frozenCount / FROZEN_COUNT;
        }

        s.lastX = cam.claimedX;
        s.lastY = cam.claimedY;
        s.lastT = t;
        s.lastHeading = cam.claimedHeading;
        s.hasPrev = true;
        // Advance the reference to a CLEAN sample (not a kinematic violation), so a single GPS
        // outlier can't pollute the reference and manufacture a second consecutive violation when
        // the vehicle moves back. Force-advance if the reference gets stale (keeps evaluating a
        // persistent attacker, whose every sample violates).
        if (!s.hasRef || (gapRef >= MIN_REF_GAP_S && (!psiViol || gapRef >= 2.0))) {
            s.refX = cam.claimedX; s.refY = cam.claimedY; s.refT = t; s.refHeading = cam.claimedHeading;
            s.refSpeed = cam.claimedSpeed;
            s.hasRef = true;
        }

        if (reason != null) {
            // Full multi-detector fingerprint (every check's normalized score, not just the first that
            // fired) — this is what a real ML-based MDS / global-MA fusion model consumes.
            Map<String, Double> det = new HashMap<>();
            det.put("acceptanceRangeThreshold", (haveSelf && ART_MAX_M > 0) ? artDist / ART_MAX_M : 0.0);
            det.put("staleOrReplay", staleSec / STALE_MAX_S);
            det.put("beaconFrequency", (double) s.winCount / FREQ_MAX);
            det.put("sybilCoLocation", (double) cellDistinct / SYBIL_MIN);
            det.put("positionJump", refReady ? movedRef / (50 + 60 * gapRef) : 0.0);
            det.put("positionSpeedInconsistency", (refReady && maxDist > 0) ? movedRef / maxDist : 0.0);
            det.put("headingInconsistency", refReady ? hd / HEADING_DIFF : 0.0);
            det.put("implausibleAcceleration",
                    refReady ? Math.abs(cam.claimedSpeed - s.refSpeed) / gapRef / MAX_PLAUSIBLE_ACCEL : 0.0);
            det.put("constantPositionFrozen", (double) s.frozenCount / FROZEN_COUNT);
            det.put("kalmanConsistency", kfNorm / KF_THRESH);
            backend.onDetection(myDigest, dg, score, t, reason, cam.posConf, scoreNorm, det);
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
