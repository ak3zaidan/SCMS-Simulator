/*
 * SPDX-License-Identifier: Apache-2.0
 * Receiver-side misbehaviour detector suite, shared by every SCMS receiver.
 *
 * Extracted verbatim from ScmsBeaconApp so that VEHICLES and ROAD-SIDE UNITS run the SAME
 * detectors over the CAMs they receive: an RSU is just a receiver that never moves and never
 * beacons, and it would be scientifically dishonest for infrastructure evidence to come from a
 * second, subtly different implementation. The class owns only per-receiver DETECTION state
 * (per-sender history + the Sybil co-location grid); channel physics (weather / NLOS / congestion
 * loss), CRL enforcement and reporting stay with the caller, because those differ between a mobile
 * and a static receiver.
 *
 * Detectors: staleOrReplay, beaconFrequency, acceptanceRangeThreshold, positionJump,
 * sybilCoLocation, positionSpeedInconsistency, headingInconsistency, implausibleAcceleration,
 * constantPositionFrozen, plus the soft kalmanConsistency score carried in the fusion fingerprint.
 */
package org.scms.app;

import java.util.HashMap;
import java.util.LinkedHashMap;
import java.util.Map;

public final class CamDetector {

    // ---- detector operating point (env-tunable; identical for vehicles and RSUs) ----
    static final double MOVING_SPEED_MS = 5.0;
    static final double FROZEN_EPS_M = 0.5;
    static final int FROZEN_COUNT = envI("SCMS_FROZEN_COUNT", 3);
    static final double ART_MAX_M = envD("SCMS_ART_MAX_M", 1000.0);
    static final double STALE_MAX_S = envD("SCMS_STALE_MAX", 5.0);
    static final int FREQ_MAX = envI("SCMS_FREQ_MAX", 12);
    static final double SPEED_TOL_M = envD("SCMS_SPEED_TOL", 10.0);
    static final double HEADING_DIFF = envD("SCMS_HEADING_DIFF", 120.0);
    static final int SYBIL_MIN = envI("SCMS_SYBIL_MIN", 5);

    static final double MIN_REF_GAP_S = 0.5;   // motion-consistency baseline (rate-independent)
    static final double MAX_ACCEL_MS2 = 9.0;   // physical accel/decel bound for plausibility
    static final int MIN_CONSEC = envI("SCMS_MIN_CONSEC", 2);   // consecutive violations before flagging
    static final double MAX_PLAUSIBLE_ACCEL = envD("SCMS_MAX_ACCEL", 12.0);   // m/s^2 (ETSI/F2MD accel check)
    static final double KF_ALPHA = 0.5, KF_BETA = 0.3;                        // alpha-beta tracker gains
    static final double KF_THRESH = envD("SCMS_KF_THRESH", 4.0);              // normalized-residual gate

    static int envI(String n, int d) {
        String e = System.getenv(n);
        try { return (e != null && !e.isBlank()) ? Integer.parseInt(e.trim()) : d; } catch (NumberFormatException ex) { return d; }
    }

    static double envD(String n, double d) {
        String e = System.getenv(n);
        try { return (e != null && !e.isBlank()) ? Double.parseDouble(e.trim()) : d; } catch (NumberFormatException ex) { return d; }
    }

    /** Per-sender receiver state (most recent CAM + a lagged >= 0.5 s motion reference + tracker). */
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

    /** One detector verdict: the firing reason plus the FULL normalized fusion fingerprint. */
    public static final class Detection {
        public final String reason;
        public final double score;
        public final double scoreNorm;
        public final Map<String, Double> detNorms;

        Detection(String reason, double score, double scoreNorm, Map<String, Double> detNorms) {
            this.reason = reason;
            this.score = score;
            this.scoreNorm = scoreNorm;
            this.detNorms = detNorms;
        }
    }

    private final Map<String, Rx> senders = new HashMap<>();
    private final Map<String, Map<String, Double>> sybilGrid = new HashMap<>();  // cell -> (digest -> lastSeen)

    /**
     * Run the whole suite over one received CAM.
     *
     * @param dg       sender pseudonym (cert digest)
     * @param cam      the received, signature-valid, non-revoked CAM
     * @param t        receiver simulation time (s)
     * @param selfX    receiver X (projected m) -- for a vehicle its own position, for an RSU its mast
     * @param selfY    receiver Y (projected m)
     * @param haveSelf false until the receiver knows where it is (disables the range check only)
     * @return a {@link Detection} when a detector fired, else null
     */
    public Detection evaluate(String dg, SignedCam cam, double t,
                              double selfX, double selfY, boolean haveSelf) {
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

        if (reason == null) {
            return null;
        }
        // Full multi-detector fingerprint (every check's normalized score, not just the first that
        // fired) — this is what a real ML-based MDS / global-MA fusion model consumes.
        // LinkedHashMap: the on-disk detnorm_* column order is INSERTION order, not JDK-dependent
        // HashMap bucket order — keeps "same seed -> byte-identical" robust across JDK versions.
        Map<String, Double> det = new LinkedHashMap<>();
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
        return new Detection(reason, score, scoreNorm, det);
    }

    static double norm360(double a) {
        return ((a % 360) + 360) % 360;
    }

    static double angleDiff(double a, double b) {
        double d = Math.abs(norm360(a) - norm360(b));
        return d > 180 ? 360 - d : d;
    }
}
